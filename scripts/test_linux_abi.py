#!/usr/bin/env python3
"""Regression tests for the release's Linux glibc compatibility gate."""

import importlib.util
from pathlib import Path
import struct
import unittest

from check_linux_abi import required_glibc_versions, verify_glibc_requirements


def elf_requirements(versions, machine=183, elf_class=2, extra=b""):
    """A small ELF with real GNU version-need records and linked string table.

    elf_class 2 builds ELF64 (x86_64, aarch64), 1 builds ELF32 (i686, armv7hf)."""
    strings = bytearray(b"\0libc.so.6\0")
    names = []
    for version in versions:
        names.append(len(strings))
        strings.extend(version.encode() + b"\0")
    needs = bytearray(struct.pack("<HHIII", 1, len(names), 1, 16, 0))
    for index, name in enumerate(names):
        needs.extend(struct.pack("<IHHII", 0, 0, index + 2, name,
                                 16 if index + 1 < len(names) else 0))
    header_size, section_size = (64, 64) if elf_class == 2 else (52, 40)
    strings_offset = header_size
    needs_offset = strings_offset + len(strings)
    extra_offset = needs_offset + len(needs)
    table_offset = extra_offset + len(extra)
    ident = b"\x7fELF" + bytes([elf_class, 1, 1]) + bytes(9)
    if elf_class == 2:
        header = struct.pack("<16sHHIQQQIHHHHHH", ident, 2, machine, 1, 0, 0, table_offset,
                             0, header_size, 0, 0, section_size, 4, 0)
        layout = "<IIQQQQIIQQ"
    else:
        header = struct.pack("<16sHHIIIIIHHHHHH", ident, 2, machine, 1, 0, 0, table_offset,
                             0, header_size, 0, 0, section_size, 4, 0)
        layout = "<IIIIIIIIII"
    sections = bytes(section_size)
    for kind, offset, size, link, info in [
        (3, strings_offset, len(strings), 0, 0),
        (0x6FFFFFFE, needs_offset, len(needs), 1, 1),
        (1, extra_offset, len(extra), 0, 0),
    ]:
        sections += struct.pack(layout, 0, kind, 0, 0, offset, size, link, info, 1, 0)
    return header + strings + needs + extra + sections


class LinuxAbiTests(unittest.TestCase):
    def test_accepts_release_baseline_and_compares_versions_numerically(self):
        data = elf_requirements(["GLIBC_2.9", "GLIBC_2.17", "GLIBC_2.28"])
        self.assertEqual(verify_glibc_requirements(data), (2, 28))

    def test_rejects_ubuntu_24_requirements_from_broken_release(self):
        for machine in [62, 183]:
            with self.subTest(machine=machine):
                data = elf_requirements(["GLIBC_2.28", "GLIBC_2.38", "GLIBC_2.39"], machine)
                with self.assertRaisesRegex(ValueError, "requires GLIBC_2.39"):
                    verify_glibc_requirements(data)

    def test_ignores_version_strings_outside_dynamic_requirements(self):
        data = elf_requirements(["GLIBC_2.28"], extra=b"GLIBC_9.99\0")
        self.assertEqual(required_glibc_versions(data), {(2, 28)})

    def test_rejects_unknown_glibc_abi_requirements(self):
        with self.assertRaisesRegex(ValueError, "unsupported glibc requirement"):
            verify_glibc_requirements(elf_requirements(["GLIBC_ABI_DT_RELR"]))

    def test_rejects_missing_truncated_and_invalid_metadata(self):
        data = elf_requirements(["GLIBC_2.28"])
        invalid_link = bytearray(data)
        table_offset = struct.unpack_from("<Q", data, 40)[0]
        struct.pack_into("<I", invalid_link, table_offset + 2 * 64 + 40, 99)
        missing_section = bytearray(data)
        struct.pack_into("<I", missing_section, table_offset + 2 * 64 + 4, 1)
        for bad in [b"", data[:63], data[:-1], bytes(invalid_link), bytes(missing_section)]:
            with self.subTest(length=len(bad)):
                with self.assertRaises(ValueError):
                    verify_glibc_requirements(bad)

    def test_reads_32_bit_executables(self):
        for machine in [3, 40]:  # i686, armv7hf
            with self.subTest(machine=machine):
                data = elf_requirements(["GLIBC_2.0", "GLIBC_2.4", "GLIBC_2.28"], machine, elf_class=1)
                self.assertEqual(verify_glibc_requirements(data), (2, 28))
                with self.assertRaisesRegex(ValueError, "requires GLIBC_2.34"):
                    verify_glibc_requirements(elf_requirements(["GLIBC_2.34"], machine, elf_class=1))

    def test_rejects_big_endian_and_unknown_elf_classes(self):
        data = bytearray(elf_requirements(["GLIBC_2.28"]))
        for index, value in [(5, 2), (4, 3)]:
            changed = bytearray(data)
            changed[index] = value
            with self.subTest(index=index), self.assertRaisesRegex(ValueError, "little-endian ELF32 or ELF64"):
                verify_glibc_requirements(bytes(changed))

    def test_packager_rejects_bad_linux_abi_before_packing(self):
        spec = importlib.util.spec_from_file_location(
            "package_apps", Path(__file__).with_name("package_apps.py")
        )
        package = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(package)
        for target, machine, elf_class in [("linux-aarch64", 183, 2), ("linux-x86_64", 62, 2),
                                           ("linux-i686", 3, 1), ("linux-armv7hf", 40, 1)]:
            with self.subTest(target=target):
                package.verify_binary(elf_requirements(["GLIBC_2.28"], machine, elf_class), target)
                with self.assertRaisesRegex(package.PackageError, "must run on glibc 2.28"):
                    package.verify_binary(elf_requirements(["GLIBC_2.39"], machine, elf_class), target)


if __name__ == "__main__":
    unittest.main()
