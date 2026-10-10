#!/usr/bin/env python3
"""Package the commit CLI for Silicon Apps, one archive per target.

    scripts/package-apps.sh VERSION TARGET BINARY [--discovery auto|require] [--receipt FILE]
    scripts/package-apps.sh 0.5.0 macos-aarch64 target/release/commit

Renders packaging/apps.yaml.in for exactly one target, stages apps.yaml and bin/commit (bin/commit.exe on
Windows), refuses a binary that is not a native executable for the target (a Linux one must also run on glibc
2.28), runs the three discovery commands Silicon Apps runs at upload, then `silicon-apps validate` and
`silicon-apps pack`, checks the archive and validates it again, and writes dist/apps/commit-VERSION-TARGET.tar.gz
with a .sha256 file beside it. It never uploads, releases or publishes anything.

Discovery: the binary must answer `commit --help`, `commit accounts --json` and `commit login status --json`
signed out, in an empty home, the way the Silicon Apps validation worker checks them. When this machine cannot
run the binary, `--receipt FILE` accepts the record `discover --receipt-out FILE` wrote on a machine that can
(it must name the same bytes); `--discovery require` (or PACKAGE_DISCOVERY=require) refuses to pack without one
of the two, which is what the release workflow uses. `auto` packs anyway and says the upload worker will check.

Other commands (the release workflow uses them):
    package_apps.py discover TARGET BINARY [--version V] [--require] [--runner JSON] [--receipt-out FILE]
    package_apps.py version [--tag vX.Y.Z]       print the release version; a tag must equal it
    package_apps.py checksums DIR --version V --expect TARGET...   check every archive, write SHA256SUMS
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import struct
import subprocess
import sys
import tarfile
import tempfile
import tomllib

sys.path.insert(0, str(Path(__file__).resolve().parent))
from check_linux_abi import verify_glibc_requirements  # noqa: E402

ROOT = Path(__file__).resolve().parents[1]
APP_ID = "commit"
COMMAND = "commit"
TEMPLATE = ROOT / "packaging" / "apps.yaml.in"
PACKER_SERIES = "0.2."
PACKER_INSTALL = "cargo install --locked silicon-apps-cli --version 0.2.0"
VERSION_PATTERN = re.compile(r"(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)")

# Silicon Apps target -> (operating system, processor, Rust target triple).
TARGETS = {
    "linux-x86_64": ("linux", "x86_64", "x86_64-unknown-linux-gnu"),
    "linux-i686": ("linux", "i686", "i686-unknown-linux-gnu"),
    "linux-aarch64": ("linux", "aarch64", "aarch64-unknown-linux-gnu"),
    "linux-armv7hf": ("linux", "armv7hf", "armv7-unknown-linux-gnueabihf"),
    "windows-x86_64": ("windows", "x86_64", "x86_64-pc-windows-msvc"),
    "windows-i686": ("windows", "i686", "i686-pc-windows-msvc"),
    "windows-aarch64": ("windows", "aarch64", "aarch64-pc-windows-msvc"),
    "macos-x86_64": ("macos", "x86_64", "x86_64-apple-darwin"),
    "macos-aarch64": ("macos", "aarch64", "aarch64-apple-darwin"),
}
# ELF EI_CLASS and e_machine, Mach-O CPU type, PE machine per processor.
ELF_MACHINE = {"x86_64": (2, 62), "i686": (1, 3), "aarch64": (2, 183), "armv7hf": (1, 40)}
MACHO_CPU = {"x86_64": 0x01000007, "aarch64": 0x0100000C}
PE_MACHINE = {"x86_64": 0x8664, "i686": 0x014C, "aarch64": 0xAA64}
DISCOVERY = (("--help",), ("accounts", "--json"), ("login", "status", "--json"))


class PackageError(Exception):
    """A refusal with the exact reason; the script prints it and exits 1."""


def binary_name(target):
    return f"{COMMAND}.exe" if TARGETS[target][0] == "windows" else COMMAND


def check_target(target):
    if target not in TARGETS:
        raise PackageError(f"unknown target {target!r}; Silicon Apps targets are {', '.join(TARGETS)}")


def check_version(version):
    if not VERSION_PATTERN.fullmatch(version):
        raise PackageError(f"version {version!r} must be strict x.y.z (no prerelease or build suffix)")


def manifest_version(path):
    """The [package] version of a Cargo manifest."""
    try:
        return tomllib.loads(path.read_text(encoding="utf-8"))["package"]["version"]
    except (OSError, KeyError, tomllib.TOMLDecodeError) as error:
        raise PackageError(f"cannot read the package version from {path}: {error}") from error


def release_version(root=ROOT, tag=None):
    """The version every manifest agrees on; with a tag, the tag must be exactly v<version>."""
    version = manifest_version(root / "cli" / "Cargo.toml")
    check_version(version)
    for manifest in ("Cargo.toml", "client/Cargo.toml"):
        found = manifest_version(root / manifest)
        if found != version:
            raise PackageError(f"{manifest} has version {found}, cli/Cargo.toml has {version}; release them together")
    cli = tomllib.loads((root / "cli" / "Cargo.toml").read_text(encoding="utf-8"))
    client = cli.get("dependencies", {}).get("silicon-commit-client", {})
    if not isinstance(client, dict) or client.get("version") != version:
        raise PackageError(f"cli/Cargo.toml must depend on silicon-commit-client version {version!r}")
    if tag is not None and tag != f"v{version}":
        raise PackageError(f"the tag {tag!r} does not match the CLI version {version}; tag the release v{version}")
    return version


def render_manifest(template, version, target):
    """apps.yaml for one target: placeholders filled in, comment lines dropped."""
    text = "".join(line for line in template.splitlines(keepends=True) if not line.lstrip().startswith("#"))
    for placeholder, value in (("@VERSION@", version), ("@TARGET@", target), ("@BINARY@", f"bin/{binary_name(target)}")):
        if placeholder not in text:
            raise PackageError(f"packaging/apps.yaml.in lost its {placeholder} placeholder")
        text = text.replace(placeholder, value)
    if "@" in text:
        raise PackageError("packaging/apps.yaml.in has a placeholder this script does not know")
    return text


def verify_binary(data, target):
    """Refuse anything but a native executable for the target; a Linux one must run on glibc 2.28."""
    system, processor, _ = TARGETS[target]
    if system == "linux":
        elf_class, machine = ELF_MACHINE[processor]
        if data[:4] != b"\x7fELF" or len(data) < 20 or data[5] != 1:
            raise PackageError(f"{target} needs a little-endian ELF executable; this file is not one")
        if data[4] != elf_class or struct.unpack_from("<H", data, 18)[0] != machine:
            raise PackageError(f"the ELF executable is not built for {target}")
        try:
            verify_glibc_requirements(data)
        except ValueError as error:
            raise PackageError(
                f"the {target} executable must run on glibc 2.28 (build it with cargo zigbuild for "
                f"{TARGETS[target][2]}.2.28, as the release workflow does): {error}"
            ) from error
    elif system == "macos":
        if data[:4] != b"\xcf\xfa\xed\xfe" or len(data) < 8:
            raise PackageError(f"{target} needs a 64-bit Mach-O executable for one processor; this file is not one")
        if struct.unpack_from("<I", data, 4)[0] != MACHO_CPU[processor]:
            raise PackageError(f"the Mach-O executable is not built for {target}")
    else:
        if data[:2] != b"MZ" or len(data) < 64:
            raise PackageError(f"{target} needs a Windows PE executable; this file is not one")
        offset = struct.unpack_from("<I", data, 60)[0]
        if len(data) < offset + 6 or data[offset:offset + 4] != b"PE\0\0":
            raise PackageError(f"{target} needs a Windows PE executable; this file has no PE header")
        if struct.unpack_from("<H", data, offset + 4)[0] != PE_MACHINE[processor]:
            raise PackageError(f"the Windows executable is not built for {target}")


def sha256(path):
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()


def host_system():
    if sys.platform.startswith("linux"):
        return "linux"
    if sys.platform == "darwin":
        return "macos"
    if sys.platform in ("win32", "cygwin", "msys"):
        return "windows"
    return sys.platform


def clean_environment(home):
    """What a validation worker gives the binary: an empty home and none of this shell's settings."""
    environment = {"HOME": str(home), "SILICON_HOME": str(home), "USERPROFILE": str(home),
                   "TMPDIR": str(home / "tmp"), "PATH": os.pathsep.join(["/usr/bin", "/bin"])}
    if host_system() == "windows":
        # Windows needs these to start a program; PATH is the system directories only.
        system_root = os.environ.get("SYSTEMROOT", r"C:\Windows")
        environment.update({"SYSTEMROOT": system_root, "WINDIR": os.environ.get("WINDIR", system_root),
                            "APPDATA": str(home), "LOCALAPPDATA": str(home),
                            "PATH": os.pathsep.join([os.path.join(system_root, "System32"), system_root])})
        environment["TEMP"] = environment["TMP"] = environment["TMPDIR"]
    return environment


class CannotRunHere(Exception):
    """This machine cannot execute the binary (another operating system or processor)."""


def run_once(binary, arguments, home, runner):
    """(exit code, stdout, stderr) of one command. A runner prefix (for example `docker run … IMAGE commit`)
    replaces the binary path and brings its own isolation, so it runs with this process's environment."""
    if runner:
        command, environment, directory = [*runner, *arguments], None, None
    else:
        (home / "tmp").mkdir(exist_ok=True)
        command, environment, directory = [str(binary), *arguments], clean_environment(home), home
    try:
        result = subprocess.run(command, cwd=directory, env=environment, capture_output=True, timeout=120,
                                check=False)
    except FileNotFoundError as error:
        raise PackageError(f"{command[0]} does not exist: {error}") from error
    except PermissionError as error:
        raise PackageError(f"{command[0]} is not executable: {error} (chmod +x it)") from error
    except OSError as error:  # Exec format error, Bad CPU type, not a valid Win32 application.
        if runner:
            raise PackageError(f"the runner {runner[0]} cannot start: {error}") from error
        raise CannotRunHere(str(error)) from error
    return result.returncode, result.stdout.decode("utf-8", "replace"), result.stderr.decode("utf-8", "replace")


def json_value(text):
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        return None


def judge(arguments, code, out, err):
    """The pass rule Silicon Apps' upload validation applies to each discovery command, and what it expects."""
    joined = " ".join(arguments)
    if joined == "--help":
        return code == 0 and bool(out.strip() or err.strip()), "exit 0 and non-empty help text"
    value = json_value(out)
    if joined == "accounts --json":
        passed = code == 0 and isinstance(value, dict) and value.get("app_id") == APP_ID
        return passed, f'exit 0 and one JSON object on stdout with "app_id": "{APP_ID}"'
    passed = code == 0 and isinstance(value, dict) and value.get("authenticated") is False
    return passed, 'exit 0 and one JSON object on stdout with "authenticated": false (signed out, empty home)'


def discover(binary, target, version=None, runner=None):
    """Run the three discovery commands signed out in an empty home; return the receipt.

    Raises PackageError when a command answers wrongly and CannotRunHere when this machine cannot run it."""
    binary = Path(binary).resolve()  # the commands run from inside the empty home
    if not binary.is_file():
        raise PackageError(f"{binary} is not a file")
    if not runner and TARGETS[target][0] != host_system():
        raise CannotRunHere(f"a {target} binary does not run on this {host_system()} machine")
    checks = []
    with tempfile.TemporaryDirectory(prefix="commit-discovery-") as directory:
        home = Path(directory)
        for arguments in DISCOVERY:
            code, out, err = run_once(binary, list(arguments), home, runner)
            passed, expected = judge(arguments, code, out, err)
            checks.append({"command": " ".join(arguments), "exit_code": code, "passed": passed})
            if not passed:
                shown = (out.strip() or err.strip())[:600]
                raise PackageError(f"`{COMMAND} {' '.join(arguments)}` must answer {expected}; "
                                   f"it exited {code} and printed {shown!r}")
        if version is not None:
            code, out, err = run_once(binary, ["--version"], home, runner)
            if code != 0 or out.split() != [COMMAND, version]:
                raise PackageError(f"`{COMMAND} --version` must print `{COMMAND} {version}` to match apps.yaml; it "
                                   f"exited {code} and printed {(out or err).strip()[:200]!r}")
    return {"app_id": APP_ID, "target": target, "version": version, "sha256": sha256(binary),
            "runner": "prefix" if runner else "native", "checks": checks}


def check_receipt(path, binary, target, version):
    """Accept the record of a discovery run on the target's own machine, for exactly these bytes."""
    try:
        receipt = json.loads(Path(path).read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise PackageError(f"cannot read the discovery receipt {path}: {error}") from error
    expected = {"app_id": APP_ID, "target": target, "version": version, "sha256": sha256(Path(binary))}
    wrong = [key for key, value in expected.items() if receipt.get(key) != value]
    if wrong:
        raise PackageError(f"the discovery receipt {path} is for other {', '.join(wrong)} "
                           f"({', '.join(f'{key}={receipt.get(key)!r}' for key in wrong)}); "
                           f"expected {', '.join(f'{key}={expected[key]!r}' for key in wrong)}")
    commands = [check.get("command") for check in receipt.get("checks", []) if check.get("passed") is True]
    if commands != [" ".join(arguments) for arguments in DISCOVERY]:
        raise PackageError(f"the discovery receipt {path} does not show all three commands passing")
    return receipt


def find_packer(explicit=None):
    candidates = [explicit, os.environ.get("SILICON_APPS"), shutil.which("silicon-apps"),
                  str(Path.home() / ".cargo" / "bin" / "silicon-apps"), str(Path.home() / ".apps" / "bin" / "silicon-apps")]
    for candidate in candidates:
        if candidate and Path(candidate).is_file():
            return candidate
    raise PackageError(f"silicon-apps is not installed: run `{PACKER_INSTALL}` or pass --silicon-apps PATH")


def packer(command, arguments, apps_home):
    """silicon-apps with an empty home of its own: validate and pack are local and never see a sign-in."""
    environment = {key: value for key, value in os.environ.items()
                   if key not in ("APPS_TOKEN", "APPS_URL", "ACCOUNTS_URL", "SILICON_HOME")}
    environment["SILICON_APPS_NO_DAEMON"] = "1"
    try:
        return subprocess.run([command, *arguments, "--home", str(apps_home), "--json"], env=environment,
                              capture_output=True, text=True, timeout=300, check=False)
    except OSError as error:
        raise PackageError(f"cannot run {command}: {error}") from error


def check_packer(command):
    try:
        found = subprocess.run([command, "--version"], capture_output=True, text=True, timeout=60,
                               check=False).stdout.split()
    except OSError as error:
        raise PackageError(f"cannot run {command}: {error}") from error
    if len(found) != 2 or found[0] != "silicon-apps" or not found[1].startswith(PACKER_SERIES):
        raise PackageError(f"{command} reports {' '.join(found) or 'no version'}; this script needs silicon-apps "
                           f"{PACKER_SERIES}x (`{PACKER_INSTALL}`)")


def validate(command, path, apps_home):
    """`silicon-apps validate` on a package directory or an archive; every error it reports is shown."""
    result = packer(command, ["validate", str(path)], apps_home)
    report = json_value(result.stdout) or {}
    if result.returncode != 0 or report.get("valid") is not True:
        raise PackageError(f"silicon-apps validate refused {path.name}: {(result.stdout or result.stderr).strip()[:2000]}")


def verify_archive(command, archive, stage, target, apps_home):
    """The archive holds exactly apps.yaml and the binary, byte for byte, and validates on its own."""
    name = binary_name(target)
    expected = {"apps.yaml", f"bin/{name}"}
    with tarfile.open(archive, "r:gz") as package:
        members = {member.name.removeprefix("./"): member for member in package.getmembers() if not member.isdir()}
        if set(members) != expected or not all(member.isfile() for member in members.values()):
            raise PackageError(f"the archive must contain exactly {sorted(expected)}; it contains {sorted(members)}")
        if TARGETS[target][0] != "windows" and not members[f"bin/{name}"].mode & 0o111:
            raise PackageError(f"bin/{name} lost its executable mode in the archive")
        for member_name, member in members.items():
            if package.extractfile(member).read() != (stage / member_name).read_bytes():
                raise PackageError(f"{member_name} in the archive differs from the staged file")
    validate(command, archive, apps_home)


def package(version, target, binary, output_dir, mode="auto", receipt=None, explicit_packer=None, root=ROOT):
    """Stage, check, validate and pack one target; returns the archive path."""
    check_target(target)
    check_version(version)
    if version != manifest_version(root / "cli" / "Cargo.toml"):
        raise PackageError(f"version {version} differs from cli/Cargo.toml "
                           f"({manifest_version(root / 'cli' / 'Cargo.toml')}); apps.yaml must match the binary")
    binary = Path(binary)
    if not binary.is_file():
        raise PackageError(f"{binary} is not a file")
    verify_binary(binary.read_bytes(), target)
    command = find_packer(explicit_packer)
    check_packer(command)
    output_dir.mkdir(parents=True, exist_ok=True)
    archive = output_dir / f"{APP_ID}-{version}-{target}.tar.gz"
    with tempfile.TemporaryDirectory(prefix="commit-package-") as directory:
        work = Path(directory)
        stage, apps_home = work / "package", work / "apps-home"
        (stage / "bin").mkdir(parents=True)
        apps_home.mkdir()
        template = (root / "packaging" / "apps.yaml.in").read_text(encoding="utf-8")
        (stage / "apps.yaml").write_text(render_manifest(template, version, target), encoding="utf-8", newline="\n")
        staged = stage / "bin" / binary_name(target)
        shutil.copyfile(binary, staged)
        staged.chmod(0o755)
        if receipt:
            check_receipt(receipt, staged, target, version)
        try:
            discover(staged, target, version)
            checked = "ran here"
        except CannotRunHere as reason:
            if receipt:
                checked = f"recorded on the {target} machine (receipt)"
            elif mode == "require":
                raise PackageError(f"the discovery commands are required, but this machine cannot run the {target} "
                                   f"binary ({reason}); pass the receipt from `discover --receipt-out` on a {target} "
                                   "machine") from None
            else:
                checked = "not run here; the Silicon Apps validation worker runs them at upload"
                print(f"note: {reason}; the Silicon Apps validation worker for {target} runs the discovery "
                      "commands at upload", file=sys.stderr)
        validate(command, stage, apps_home)
        temporary = work / archive.name
        result = packer(command, ["pack", str(stage), "--output", str(temporary)], apps_home)
        if result.returncode != 0 or not temporary.is_file():
            raise PackageError(f"silicon-apps pack failed: {(result.stdout or result.stderr).strip()[:2000]}")
        verify_archive(command, temporary, stage, target, apps_home)
        shutil.move(str(temporary), archive)
    digest = sha256(archive)
    Path(f"{archive}.sha256").write_text(f"{digest}  {archive.name}\n", encoding="utf-8", newline="\n")
    print(f"packaged {archive} ({archive.stat().st_size} bytes)\nsha256 {digest}\ndiscovery {checked}")
    return archive


def checksums(directory, version, expected_targets):
    """Check each expected archive against its .sha256 file and write SHA256SUMS for every archive."""
    lines = []
    for target in expected_targets:
        check_target(target)
        name = f"{APP_ID}-{version}-{target}.tar.gz"
        if not (directory / name).is_file():
            raise PackageError(f"{name} is missing from {directory}; every release target must be packed")
    for archive in sorted(directory.glob(f"{APP_ID}-*.tar.gz")):
        digest = sha256(archive)
        sidecar = archive.with_name(archive.name + ".sha256")
        if not sidecar.is_file() or sidecar.read_text(encoding="utf-8").split() != [digest, archive.name]:
            raise PackageError(f"{sidecar.name} does not match {archive.name} (sha256 {digest})")
        lines.append(f"{digest}  {archive.name}\n")
    (directory / "SHA256SUMS").write_text("".join(lines), encoding="utf-8", newline="\n")
    print("".join(lines), end="")
    return lines


def parse_runner(text):
    try:
        runner = json.loads(text)
    except json.JSONDecodeError as error:
        raise PackageError(f"--runner must be a JSON array of strings: {error}") from error
    if not isinstance(runner, list) or not runner or not all(isinstance(part, str) for part in runner):
        raise PackageError("--runner must be a non-empty JSON array of strings, the command that runs the binary")
    return runner


def main(argv=None):
    argv = list(sys.argv[1:] if argv is None else argv)
    command = argv[0] if argv and argv[0] in ("discover", "version", "checksums") else "package"
    if command == "package":
        parser = argparse.ArgumentParser(prog="scripts/package-apps.sh", description=__doc__,
                                         formatter_class=argparse.RawDescriptionHelpFormatter)
        parser.add_argument("version", help="the release version, equal to cli/Cargo.toml (for example 0.5.0)")
        parser.add_argument("target", help="one Silicon Apps target, for example linux-x86_64 or macos-aarch64")
        parser.add_argument("binary", type=Path, help="the built commit executable for that target")
        parser.add_argument("--output-dir", type=Path, default=ROOT / "dist" / "apps", help="default: dist/apps")
        parser.add_argument("--discovery", choices=("auto", "require"),
                            default=os.environ.get("PACKAGE_DISCOVERY") or "auto",
                            help="require: refuse to pack without a discovery run here or a receipt (default auto)")
        parser.add_argument("--receipt", type=Path, help="discovery receipt written on the target's own machine")
        parser.add_argument("--silicon-apps", help="the silicon-apps executable (default: SILICON_APPS, then PATH)")
    else:
        argv = argv[1:]
        parser = argparse.ArgumentParser(prog=f"package_apps.py {command}")
        if command == "discover":
            parser.add_argument("target")
            parser.add_argument("binary", type=Path)
            parser.add_argument("--version", help="also require `commit --version` to print this version")
            parser.add_argument("--require", action="store_true", help="fail when this machine cannot run it")
            parser.add_argument("--runner", help='JSON array that runs the binary, e.g. ["docker","run",…,"commit"]')
            parser.add_argument("--receipt-out", type=Path, help="write the receipt the packager accepts")
        elif command == "version":
            parser.add_argument("--tag", help="the release tag; it must be exactly v<version>")
        else:
            parser.add_argument("directory", type=Path)
            parser.add_argument("--version", required=True)
            parser.add_argument("--expect", nargs="+", required=True, metavar="TARGET")
    args = parser.parse_args(argv)
    try:
        if command == "package":
            if args.discovery not in ("auto", "require"):
                parser.error(f"PACKAGE_DISCOVERY must be auto or require, not {args.discovery!r}")
            package(args.version, args.target, args.binary, args.output_dir.resolve(), args.discovery,
                    args.receipt, args.silicon_apps)
        elif command == "discover":
            check_target(args.target)
            if args.version:
                check_version(args.version)
            runner = parse_runner(args.runner) if args.runner else None
            try:
                receipt = discover(args.binary, args.target, args.version, runner)
            except CannotRunHere as reason:
                if args.require:
                    raise PackageError(f"cannot run the {args.target} binary here: {reason}") from None
                print(f"note: {reason}; nothing was checked", file=sys.stderr)
                return 0
            if args.receipt_out:
                args.receipt_out.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
            print(f"discovery: --help, accounts --json and login status --json answered as Silicon Apps requires "
                  f"({args.target}, {receipt['runner']})")
        elif command == "version":
            print(release_version(tag=args.tag))
        else:
            check_version(args.version)
            checksums(args.directory, args.version, args.expect)
    except (PackageError, OSError, subprocess.SubprocessError) as error:
        print(f"package-apps: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
