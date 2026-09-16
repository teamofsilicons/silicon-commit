# Commit 0.2.0 Honeycomb package

Built locally on 2026-09-16 for `tos>commit` using Rust 1.98.0 and
`scripts/build-release-macos.sh`. The archive is `dist/commit-0.2.0.tar.gz`
(14,543,413 bytes); individual binaries are in `dist/binaries/`.
Build outputs are local artifacts, excluded from Git.

Honeycomb 0.1.0 validated the staging directory and final archive. The archive
contains the root `honeycomb.yaml` and exactly six native executables.

| Targets | Verification |
| --- | --- |
| macOS x86_64 and aarch64 | Packaged binaries passed version, help and daemon-status smoke checks; x86_64 used Rosetta. |
| Linux x86_64 and aarch64 | Packaged binaries passed the same checks in isolated Debian Bookworm containers; x86_64 used emulation. ELF imports require no glibc version above 2.28. |
| Windows x86_64 and aarch64 | PE architecture and DLL imports checked; Visual C++ runtime linked statically. Execution on Windows is still unverified. |

Cross-linkers reported non-fatal deprecated optimization and missing CRT debug
information warnings. The CI workflow in [RELEASES.md](../RELEASES.md) provides
native Windows runtime checks before publication.

SHA-256 checksums (archive, then files inside the archive):

```text
dbe42813a163a31be1cfe2c75cd0faf846a06e1116af45c5b0aa61b3478d5396  commit-0.2.0.tar.gz
25a6b750ed9a5eb5960d9ad662fb2cf5a45671bf5bcf109c0331686aa98ee4d0  targets/linux-x86_64/bin/commit
7be65c8b8a54d42ff69df0ec126dfdb67b4d275a620743aa3f202256e74ff5b8  targets/linux-aarch64/bin/commit
73eb7a02252b427881a595652ddc7d10605a30cb92b35cbc6576509d8ab719cf  targets/windows-x86_64/bin/commit.exe
fe00ec842300d3f1f0149f05a3429a6c3675cc7ced9827e554e4dd3b22d39f08  targets/windows-aarch64/bin/commit.exe
c49a5b005e553047c769ca501809ffeb1368e71a85ab43ce616f45ba63d0729d  targets/macos-x86_64/bin/commit
c38f58f04604ffe64c4fea74ccc2bbbf3b657225b5c6ebd2fd369cb11bed4abe  targets/macos-aarch64/bin/commit
```
