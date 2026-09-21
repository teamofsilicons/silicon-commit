#!/usr/bin/env python3
"""Stage six prebuilt managed CLIs and validate/pack a Honeycomb release.

Local builds are read from target/<triple>/release. CI can pass
--binaries-dir containing <honeycomb-target>/commit[.exe] instead.
Requires Python 3.11+ and the Honeycomb CLI; never adds source or configuration.
"""

import argparse
import hashlib
from pathlib import Path
import re
import shutil
import struct
import subprocess
import tarfile
import tempfile
import tomllib

from check_linux_abi import verify_glibc_requirements


ROOT = Path(__file__).resolve().parents[1]
TARGETS = {
    "linux-x86_64": "x86_64-unknown-linux-gnu",
    "linux-aarch64": "aarch64-unknown-linux-gnu",
    "windows-x86_64": "x86_64-pc-windows-msvc",
    "windows-aarch64": "aarch64-pc-windows-msvc",
    "macos-x86_64": "x86_64-apple-darwin",
    "macos-aarch64": "aarch64-apple-darwin",
}


def verify_binary(path: Path, target: str) -> None:
    """Reject scripts, wrong operating systems and wrong CPU architectures."""
    data = path.read_bytes()
    arm = target.endswith("aarch64")
    if target.startswith("linux-"):
        valid = (
            data[:6] == b"\x7fELF\x02\x01"
            and len(data) >= 20
            and struct.unpack_from("<H", data, 18)[0] == (183 if arm else 62)
        )
    elif target.startswith("macos-"):
        valid = (
            data[:4] == b"\xcf\xfa\xed\xfe"
            and len(data) >= 8
            and struct.unpack_from("<I", data, 4)[0]
            == (0x0100000C if arm else 0x01000007)
        )
    else:
        valid = False
        if data[:2] == b"MZ" and len(data) >= 64:
            offset = struct.unpack_from("<I", data, 60)[0]
            valid = (
                len(data) >= offset + 6
                and data[offset : offset + 4] == b"PE\0\0"
                and struct.unpack_from("<H", data, offset + 4)[0]
                == (0xAA64 if arm else 0x8664)
            )
    if not valid:
        raise SystemExit(f"Wrong native binary format or architecture for {target}: {path}")
    if target.startswith("linux-"):
        try:
            verify_glibc_requirements(data)
        except ValueError as error:
            raise SystemExit(f"Unsupported Linux ABI for {target}: {path}: {error}") from error


def run(*command: str) -> None:
    print("+", " ".join(map(str, command)), flush=True)
    subprocess.run(command, check=True)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--honeycomb", default="honeycomb", help="Honeycomb CLI executable")
    parser.add_argument("--binaries-dir", type=Path, help="CI artifact directory")
    parser.add_argument("--output-dir", type=Path, default=ROOT / "dist")
    args = parser.parse_args()
    version = tomllib.loads((ROOT / "cli/Cargo.toml").read_text())["package"]["version"]
    for cargo_manifest in ("Cargo.toml", "client/Cargo.toml"):
        if tomllib.loads((ROOT / cargo_manifest).read_text())["package"]["version"] != version:
            raise SystemExit(f"{cargo_manifest} must use app release version {version}")
    manifest = (ROOT / "honeycomb.yaml").read_text()
    for key, expected in (("app_id", "tos>commit"), ("version", version)):
        match = re.search(rf"^{key}:\s*[\"']?([^\s\"'#]+)[\"']?\s*(?:#.*)?$", manifest, re.MULTILINE)
        if not match or match.group(1) != expected:
            raise SystemExit(f"honeycomb.yaml {key} must match {expected!r}")

    output = args.output_dir.resolve()
    output.mkdir(parents=True, exist_ok=True)
    archive = output / f"commit-{version}.tar.gz"
    checksums = []
    expected_files = {"honeycomb.yaml"}
    with tempfile.TemporaryDirectory(prefix="commit-release-") as temporary:
        stage = Path(temporary)
        shutil.copyfile(ROOT / "honeycomb.yaml", stage / "honeycomb.yaml")
        for target, triple in TARGETS.items():
            name = "commit.exe" if target.startswith("windows-") else "commit"
            source = (
                args.binaries_dir / target / name
                if args.binaries_dir
                else ROOT / "target" / triple / "release" / name
            )
            if not source.is_file():
                raise SystemExit(f"Missing {target} binary: {source}. Build all six native targets before packaging.")
            verify_binary(source, target)
            relative = Path("targets") / target / "bin" / name
            destination = stage / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
            destination.chmod(0o755)
            expected_files.add(relative.as_posix())
            checksums.append(f"{hashlib.sha256(destination.read_bytes()).hexdigest()}  {relative.as_posix()}")
        # Both commands are mandatory: validation before packing, and validation
        # of the resulting archive rather than relying on the staging result.
        run(args.honeycomb, "validate", str(stage))
        run(args.honeycomb, "pack", str(stage), "--output", str(archive))
        run(args.honeycomb, "validate", str(archive))

    with tarfile.open(archive, "r:gz") as package:
        members = package.getmembers()
        files = {member.name.removeprefix("./") for member in members if member.isfile()}
        if files != expected_files or any(not (member.isfile() or member.isdir()) for member in members):
            raise SystemExit("Archive must contain only the manifest and six native executables")
    digest = hashlib.sha256(archive.read_bytes()).hexdigest()
    (output / f"{archive.name}.sha256").write_text(f"{digest}  {archive.name}\n")
    (output / f"commit-{version}-binaries.sha256").write_text("\n".join(checksums) + "\n")
    print(f"Validated release: {archive}\nSHA-256: {digest}")


if __name__ == "__main__":
    main()
