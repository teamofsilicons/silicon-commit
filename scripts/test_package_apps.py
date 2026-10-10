#!/usr/bin/env python3
"""Tests for the Silicon Apps packager: fake binaries and a fake silicon-apps, no network."""

import hashlib
import io
import json
import os
from pathlib import Path
import struct
import sys
import tarfile
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import package_apps as apps  # noqa: E402
from test_linux_abi import elf_requirements  # noqa: E402

VERSION = apps.manifest_version(apps.ROOT / "cli" / "Cargo.toml")
HOST = apps.host_system()
HOST_TARGET = {"linux": "linux-x86_64", "macos": "macos-aarch64", "windows": "windows-x86_64"}.get(HOST)
OTHER_TARGET = "macos-aarch64" if HOST == "linux" else "linux-x86_64"
UNIX = os.name != "nt" and HOST_TARGET is not None

FAKE_PACKER = r'''
import io, json, os, sys, tarfile
from pathlib import Path
args = sys.argv[1:]
if os.environ.get("FAKE_PACKER_LOG"):
    with open(os.environ["FAKE_PACKER_LOG"], "a") as log:
        seen = {k: os.environ.get(k) for k in ("SILICON_HOME", "APPS_TOKEN", "SILICON_APPS_NO_DAEMON")}
        log.write(json.dumps({"args": args, "env": seen}) + "\n")
if args == ["--version"]:
    print("silicon-apps " + os.environ.get("FAKE_PACKER_VERSION", "0.2.0"))
    sys.exit(0)
command, path = args[0], Path(args[1])
home = Path(args[args.index("--home") + 1])
if not home.is_dir():
    sys.exit(3)
if command == "validate":
    valid = os.environ.get("FAKE_PACKER_INVALID") != "1"
    if path.is_dir():
        valid = valid and (path / "apps.yaml").is_file()
    else:
        with tarfile.open(path) as archive:
            valid = valid and "apps.yaml" in archive.getnames()
    print(json.dumps({"valid": valid, "errors": [] if valid else ["fake refusal: apps.yaml is wrong"]}))
    sys.exit(0 if valid else 1)
if command == "pack":
    output = Path(args[args.index("--output") + 1])
    with tarfile.open(output, "w:gz") as archive:
        for file in sorted(p for p in path.rglob("*") if p.is_file()):
            archive.add(file, arcname=file.relative_to(path).as_posix())
        if os.environ.get("FAKE_PACKER_EXTRA") == "1":
            info = tarfile.TarInfo("extra.txt")
            info.size = 5
            archive.addfile(info, io.BytesIO(b"extra"))
    print(json.dumps({"path": str(output)}))
    sys.exit(0)
sys.exit(2)
'''


def fake_cli(directory, accounts='{"app_id":"commit"}', status='{\n  "authenticated": false\n}',
             version=None, help_exit=0, prelude=""):
    """A shell script that answers the discovery commands the way it is told to."""
    script = Path(directory) / "commit"
    script.write_text(f"""#!/bin/sh
{prelude}
case "$*" in
  "--help") echo "commit: keeps todos and projects"; exit {help_exit} ;;
  "accounts --json") printf '%s\\n' '{accounts}' ;;
  "login status --json") printf '%s\\n' '{status}' ;;
  "--version") echo "commit {version or VERSION}" ;;
  *) echo "commit: unknown command: $*" >&2; exit 2 ;;
esac
""", encoding="utf-8")
    script.chmod(0o755)
    return script


def mach_o(cpu):
    return b"\xcf\xfa\xed\xfe" + struct.pack("<I", cpu) + bytes(24)


def portable_executable(machine):
    return b"MZ" + bytes(58) + struct.pack("<I", 64) + b"PE\0\0" + struct.pack("<H", machine) + bytes(16)


class ManifestAndBinaryTests(unittest.TestCase):
    def test_manifest_lists_only_its_target_without_comments(self):
        template = apps.TEMPLATE.read_text(encoding="utf-8")
        self.assertEqual(
            apps.render_manifest(template, "1.2.3", "linux-aarch64"),
            "schema_version: 1\napp_id: commit\nversion: 1.2.3\ncommand: commit\ntargets:\n"
            "  linux-aarch64:\n    binary: bin/commit\n",
        )
        self.assertIn("binary: bin/commit.exe\n", apps.render_manifest(template, "1.2.3", "windows-i686"))

    def test_manifest_template_must_keep_its_placeholders(self):
        with self.assertRaisesRegex(apps.PackageError, "@TARGET@"):
            apps.render_manifest("version: @VERSION@\nbinary: @BINARY@\n", "1.2.3", "linux-x86_64")
        with self.assertRaisesRegex(apps.PackageError, "does not know"):
            apps.render_manifest("@VERSION@ @TARGET@ @BINARY@ @CHANNEL@\n", "1.2.3", "linux-x86_64")

    def test_linux_executables_must_match_the_processor_and_glibc_2_28(self):
        for target, (elf_class, machine) in [("linux-x86_64", (2, 62)), ("linux-aarch64", (2, 183)),
                                             ("linux-i686", (1, 3)), ("linux-armv7hf", (1, 40))]:
            with self.subTest(target=target):
                apps.verify_binary(elf_requirements(["GLIBC_2.17", "GLIBC_2.28"], machine, elf_class), target)
                with self.assertRaisesRegex(apps.PackageError, "must run on glibc 2.28"):
                    apps.verify_binary(elf_requirements(["GLIBC_2.39"], machine, elf_class), target)
        with self.assertRaisesRegex(apps.PackageError, "not built for linux-aarch64"):
            apps.verify_binary(elf_requirements(["GLIBC_2.28"], 62), "linux-aarch64")
        with self.assertRaisesRegex(apps.PackageError, "not built for linux-i686"):
            apps.verify_binary(elf_requirements(["GLIBC_2.28"], 3, 2), "linux-i686")

    def test_macos_and_windows_executables_must_match_the_processor(self):
        apps.verify_binary(mach_o(0x0100000C), "macos-aarch64")
        apps.verify_binary(mach_o(0x01000007), "macos-x86_64")
        with self.assertRaisesRegex(apps.PackageError, "not built for macos-x86_64"):
            apps.verify_binary(mach_o(0x0100000C), "macos-x86_64")
        with self.assertRaisesRegex(apps.PackageError, "one processor"):
            apps.verify_binary(b"\xca\xfe\xba\xbe" + bytes(28), "macos-aarch64")  # universal binary
        apps.verify_binary(portable_executable(0x8664), "windows-x86_64")
        apps.verify_binary(portable_executable(0x014C), "windows-i686")
        apps.verify_binary(portable_executable(0xAA64), "windows-aarch64")
        with self.assertRaisesRegex(apps.PackageError, "not built for windows-aarch64"):
            apps.verify_binary(portable_executable(0x8664), "windows-aarch64")

    def test_scripts_are_not_native_executables(self):
        for target in apps.TARGETS:
            with self.subTest(target=target), self.assertRaises(apps.PackageError):
                apps.verify_binary(b"#!/bin/sh\necho commit\n" + bytes(64), target)


class VersionAndChecksumTests(unittest.TestCase):
    def project(self, cli="1.4.0", client="1.4.0", root="1.4.0", dependency="1.4.0"):
        directory = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(directory))
        (directory / "cli").mkdir()
        (directory / "client").mkdir()
        (directory / "Cargo.toml").write_text(f'[package]\nname = "silicon-commit"\nversion = "{root}"\n')
        (directory / "client" / "Cargo.toml").write_text(f'[package]\nname = "c"\nversion = "{client}"\n')
        (directory / "cli" / "Cargo.toml").write_text(
            f'[package]\nname = "silicon-commit-cli"\nversion = "{cli}"\n\n[dependencies]\n'
            f'silicon-commit-client = {{ version = "{dependency}", path = "../client" }}\n')
        return directory

    def test_every_manifest_and_the_tag_must_agree(self):
        self.assertEqual(apps.release_version(self.project()), "1.4.0")
        self.assertEqual(apps.release_version(self.project(), tag="v1.4.0"), "1.4.0")
        with self.assertRaisesRegex(apps.PackageError, "tag 'v1.4.1' does not match"):
            apps.release_version(self.project(), tag="v1.4.1")
        with self.assertRaisesRegex(apps.PackageError, "client/Cargo.toml has version 1.3.0"):
            apps.release_version(self.project(client="1.3.0"))
        with self.assertRaisesRegex(apps.PackageError, "depend on silicon-commit-client version"):
            apps.release_version(self.project(dependency="1.3.0"))
        with self.assertRaisesRegex(apps.PackageError, "strict x.y.z"):
            apps.release_version(self.project(cli="1.4.0-rc.1", client="1.4.0-rc.1", root="1.4.0-rc.1"))

    def test_this_repository_releases_one_version(self):
        self.assertEqual(apps.release_version(), VERSION)

    def test_checksums_cover_every_archive_and_refuse_gaps(self):
        with tempfile.TemporaryDirectory() as name:
            directory = Path(name)
            for target in ("linux-x86_64", "macos-aarch64"):
                archive = directory / f"commit-1.0.0-{target}.tar.gz"
                archive.write_bytes(target.encode())
                digest = hashlib.sha256(target.encode()).hexdigest()
                (directory / f"{archive.name}.sha256").write_text(f"{digest}  {archive.name}\n")
            with redirect_stdout(io.StringIO()):
                lines = apps.checksums(directory, "1.0.0", ["linux-x86_64", "macos-aarch64"])
            self.assertEqual(len(lines), 2)
            self.assertEqual((directory / "SHA256SUMS").read_text(), "".join(lines))
            with self.assertRaisesRegex(apps.PackageError, "commit-1.0.0-linux-aarch64.tar.gz is missing"):
                apps.checksums(directory, "1.0.0", ["linux-aarch64"])
            (directory / "commit-1.0.0-linux-x86_64.tar.gz").write_bytes(b"changed")
            with redirect_stdout(io.StringIO()), self.assertRaisesRegex(apps.PackageError, "does not match"):
                apps.checksums(directory, "1.0.0", ["linux-x86_64"])


@unittest.skipUnless(UNIX, "the fake binaries are shell scripts")
class DiscoveryTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.directory))

    def test_a_correct_binary_passes_and_yields_a_receipt_for_its_bytes(self):
        binary = fake_cli(self.directory)
        receipt = apps.discover(binary, HOST_TARGET, VERSION)
        self.assertEqual(receipt["sha256"], hashlib.sha256(binary.read_bytes()).hexdigest())
        self.assertEqual([c["command"] for c in receipt["checks"]], ["--help", "accounts --json", "login status --json"])
        self.assertTrue(all(c["passed"] and c["exit_code"] == 0 for c in receipt["checks"]))
        self.assertEqual((receipt["target"], receipt["version"], receipt["runner"]), (HOST_TARGET, VERSION, "native"))

    def test_each_wrong_answer_is_refused_with_what_was_expected(self):
        cases = [
            (dict(help_exit=1), "commit --help` must answer exit 0"),
            (dict(accounts='{"app_id":"commit-dev"}'), '"app_id": "commit"'),
            (dict(accounts="app_id=commit"), "accounts --json` must answer"),
            (dict(accounts='["commit"]'), "accounts --json` must answer"),
            (dict(status='{"authenticated":true,"id":"c:ada"}'), '"authenticated": false'),
            (dict(status='{"account":null}'), '"authenticated": false'),
            (dict(status='{"authenticated":"false"}'), '"authenticated": false'),
            (dict(status="signed out"), "login status --json` must answer"),
            (dict(version="0.0.1"), "--version` must print"),
        ]
        for options, message in cases:
            with self.subTest(options=options), self.assertRaisesRegex(apps.PackageError, message):
                apps.discover(fake_cli(self.directory, **options), HOST_TARGET, VERSION)

    def test_commands_run_in_an_empty_home_without_this_shell_environment(self):
        prelude = '\n'.join([
            '[ -z "$COMMIT_ACCESS_TOKEN" ] || exit 7',
            '[ "$HOME" = "$SILICON_HOME" ] || exit 8',
            '[ "$(pwd -P)" = "$(cd "$SILICON_HOME" && pwd -P)" ] || exit 9',
            '[ -z "$(ls -A "$SILICON_HOME" | grep -v "^tmp$")" ] || exit 10',
        ])
        binary = fake_cli(self.directory, prelude=prelude)
        with mock.patch.dict(os.environ, {"COMMIT_ACCESS_TOKEN": "leaked", "SILICON_HOME": str(self.directory)}):
            apps.discover(binary, HOST_TARGET)

    def test_a_runner_prefix_runs_the_commands_and_a_missing_runner_is_an_error(self):
        binary = fake_cli(self.directory)
        receipt = apps.discover(binary, OTHER_TARGET, VERSION, runner=["/bin/sh", str(binary)])
        self.assertEqual(receipt["runner"], "prefix")
        with self.assertRaisesRegex(apps.PackageError, "does not exist"):
            apps.discover(binary, OTHER_TARGET, runner=[str(self.directory / "no-such-runner")])

    def test_binaries_this_machine_cannot_run_are_reported_as_such(self):
        with self.assertRaises(apps.CannotRunHere):
            apps.discover(fake_cli(self.directory), OTHER_TARGET)
        garbage = self.directory / "garbage"
        garbage.write_bytes(b"\x7fELF\x02\x01\x01" + bytes(200))
        garbage.chmod(0o755)
        with self.assertRaises(apps.CannotRunHere):
            apps.discover(garbage, HOST_TARGET)
        with self.assertRaisesRegex(apps.PackageError, "is not a file"):
            apps.discover(self.directory / "missing", HOST_TARGET)

    def test_receipts_must_name_the_same_bytes_target_and_version(self):
        binary = fake_cli(self.directory)
        receipt = apps.discover(binary, HOST_TARGET, VERSION)
        path = self.directory / "receipt.json"
        path.write_text(json.dumps(receipt))
        apps.check_receipt(path, binary, HOST_TARGET, VERSION)
        for change, message in [({"sha256": "0" * 64}, "other sha256"), ({"target": OTHER_TARGET}, "other target"),
                                ({"version": "9.9.9"}, "other version"),
                                ({"checks": receipt["checks"][:2]}, "all three commands")]:
            with self.subTest(change=change):
                path.write_text(json.dumps({**receipt, **change}))
                with self.assertRaisesRegex(apps.PackageError, message):
                    apps.check_receipt(path, binary, HOST_TARGET, VERSION)
        path.write_text("{")
        with self.assertRaisesRegex(apps.PackageError, "cannot read the discovery receipt"):
            apps.check_receipt(path, binary, HOST_TARGET, VERSION)


@unittest.skipUnless(UNIX, "the fake binaries and packer are shell scripts")
class PackageTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path(tempfile.mkdtemp())
        self.addCleanup(lambda: __import__("shutil").rmtree(self.directory))
        program = self.directory / "fake_packer.py"
        program.write_text(FAKE_PACKER, encoding="utf-8")
        self.packer = self.directory / "silicon-apps"
        self.packer.write_text(f'#!/bin/sh\nexec "{sys.executable}" "{program}" "$@"\n', encoding="utf-8")
        self.packer.chmod(0o755)
        self.log = self.directory / "packer.log"
        self.output = self.directory / "dist"
        patcher = mock.patch.object(apps, "verify_binary")  # the fake binary is a script; tested above
        patcher.start()
        self.addCleanup(patcher.stop)
        environment = mock.patch.dict(os.environ, {"FAKE_PACKER_LOG": str(self.log), "APPS_TOKEN": "secret",
                                                   "SILICON_HOME": str(self.directory)})
        environment.start()
        self.addCleanup(environment.stop)

    def pack(self, target=HOST_TARGET, mode="auto", receipt=None, binary=None, version=VERSION):
        with redirect_stdout(io.StringIO()) as out, redirect_stderr(io.StringIO()) as err:
            archive = apps.package(version, target, binary or fake_cli(self.directory), self.output, mode, receipt,
                                   str(self.packer))
        return archive, out.getvalue(), err.getvalue()

    def test_packs_exactly_the_manifest_and_the_executable(self):
        archive, out, _ = self.pack()
        self.assertEqual(archive.name, f"commit-{VERSION}-{HOST_TARGET}.tar.gz")
        self.assertIn("discovery ran here", out)
        with tarfile.open(archive) as package:
            members = {member.name: member for member in package.getmembers()}
            self.assertEqual(set(members), {"apps.yaml", "bin/commit"})
            self.assertTrue(members["bin/commit"].mode & 0o111)
            manifest = package.extractfile("apps.yaml").read().decode()
        self.assertIn(f"version: {VERSION}\n", manifest)
        self.assertIn(f"  {HOST_TARGET}:\n    binary: bin/commit\n", manifest)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        self.assertEqual(Path(f"{archive}.sha256").read_text(), f"{digest}  {archive.name}\n")
        calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        self.assertEqual([call["args"][0] for call in calls], ["--version", "validate", "pack", "validate"])
        for call in calls[1:]:
            self.assertIn("--home", call["args"])
            self.assertEqual(call["env"], {"SILICON_HOME": None, "APPS_TOKEN": None, "SILICON_APPS_NO_DAEMON": "1"})
        self.assertTrue(calls[3]["args"][1].endswith(".tar.gz"), "the archive itself is validated again")

    def test_binaries_that_answer_wrongly_are_never_packed(self):
        with self.assertRaisesRegex(apps.PackageError, "authenticated"):
            self.pack(binary=fake_cli(self.directory, status='{"authenticated":true}'))
        self.assertFalse(self.output.exists() and any(self.output.iterdir()))

    def test_other_targets_need_a_receipt_when_discovery_is_required(self):
        with self.assertRaisesRegex(apps.PackageError, "cannot run the .* binary"):
            self.pack(OTHER_TARGET, "require")
        binary = fake_cli(self.directory)
        receipt = self.directory / "receipt.json"
        receipt.write_text(json.dumps(apps.discover(binary, OTHER_TARGET, VERSION, runner=["/bin/sh", str(binary)])
                                      | {"runner": "native"}))
        _, out, _ = self.pack(OTHER_TARGET, "require", receipt, binary)
        self.assertIn("recorded on the", out)
        receipt.write_text(json.dumps(json.loads(receipt.read_text()) | {"sha256": "1" * 64}))
        with self.assertRaisesRegex(apps.PackageError, "other sha256"):
            self.pack(OTHER_TARGET, "require", receipt, binary)
        _, out, err = self.pack(OTHER_TARGET, "auto")
        self.assertIn("validation worker", err)
        self.assertIn("discovery not run here", out)

    def test_inputs_and_packer_problems_are_refused_with_the_reason(self):
        with self.assertRaisesRegex(apps.PackageError, "differs from cli/Cargo.toml"):
            self.pack(version="9.9.9")
        with self.assertRaisesRegex(apps.PackageError, "strict x.y.z"):
            self.pack(version=f"{VERSION}-rc.1")
        with self.assertRaisesRegex(apps.PackageError, "unknown target"):
            self.pack(target="linux-riscv64")
        with self.assertRaisesRegex(apps.PackageError, "is not a file"):
            self.pack(binary=self.directory / "missing")
        with mock.patch.dict(os.environ, {"FAKE_PACKER_VERSION": "0.1.9"}), \
                self.assertRaisesRegex(apps.PackageError, "needs silicon-apps 0.2.x"):
            self.pack()
        with mock.patch.dict(os.environ, {"FAKE_PACKER_INVALID": "1"}), \
                self.assertRaisesRegex(apps.PackageError, "fake refusal: apps.yaml is wrong"):
            self.pack()
        with mock.patch.dict(os.environ, {"FAKE_PACKER_EXTRA": "1"}), \
                self.assertRaisesRegex(apps.PackageError, "must contain exactly"):
            self.pack()


class CommandLineTests(unittest.TestCase):
    def run_main(self, *argv):
        with redirect_stdout(io.StringIO()) as out, redirect_stderr(io.StringIO()) as err:
            code = apps.main(list(argv))
        return code, out.getvalue(), err.getvalue()

    @unittest.skipUnless(UNIX, "the fake binary is a shell script")
    def test_discover_requires_a_runnable_binary_only_when_asked(self):
        with tempfile.TemporaryDirectory() as name:
            binary = fake_cli(name)
            self.assertEqual(self.run_main("discover", OTHER_TARGET, str(binary))[0], 0)
            code, _, err = self.run_main("discover", OTHER_TARGET, str(binary), "--require")
            self.assertEqual(code, 1)
            self.assertIn("cannot run", err)
            receipt = Path(name) / "receipt.json"
            code, out, _ = self.run_main("discover", HOST_TARGET, str(binary), "--version", VERSION,
                                         "--require", "--receipt-out", str(receipt))
            self.assertEqual((code, json.loads(receipt.read_text())["target"]), (0, HOST_TARGET))
            self.assertIn("answered as Silicon Apps requires", out)

    def test_usage_errors_exit_1_with_the_reason(self):
        code, _, err = self.run_main("discover", "linux-x86_64", "commit", "--runner", "docker run")
        self.assertEqual(code, 1)
        self.assertIn("JSON array", err)
        code, _, err = self.run_main("version", "--tag", "v0.0.0")
        self.assertEqual(code, 1)
        self.assertIn("does not match the CLI version", err)
        self.assertEqual(self.run_main("version")[1].strip(), VERSION)


if __name__ == "__main__":
    unittest.main()
