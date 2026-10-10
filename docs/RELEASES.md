# Release Commit

Commit's CLI reaches Carbons and Silicons through Silicon Apps: one `.tar.gz` per target, each with `apps.yaml` at
its root. Silicon Apps installs it (`silicon-apps install commit`) and its updater keeps every installed copy
current; the CLI never replaces itself. The Rust client is a normal Cargo dependency on crates.io.

## Cut a release

1. Give the root, client and CLI manifests the same version (`Cargo.toml`, `client/Cargo.toml`,
   `cli/Cargo.toml`, and the CLI's dependency on `silicon-commit-client`). Breaking changes take the next minor
   version while Commit is below 1.0.
2. Tag the release commit `v<version>` and push the tag. The **release** workflow (`.github/workflows/release.yml`)
   checks that the tag equals the version, builds every target, packs the archives and uploads them as the
   workflow artifact `commit-silicon-apps-release`: `commit-<version>-<target>.tar.gz` for each target, a
   `.sha256` beside each, `SHA256SUMS` and `SOURCE_REVISION`. It publishes nothing. You can also start it by hand
   (**Run workflow**) to build archives from a branch.
3. Download the artifact and check it: `shasum -a 256 -c SHA256SUMS`.
4. Upload, release and promote with the `silicon-apps` CLI, signed in as one of the app's authors. Upload one
   archive per target, then make a development release from the accepted packages, try it, and promote it:

   ```sh
   silicon-apps capabilities                    # which validation workers are live
   silicon-apps upload commit --target linux-x86_64 commit-0.5.0-linux-x86_64.tar.gz
   silicon-apps upload commit --target linux-aarch64 commit-0.5.0-linux-aarch64.tar.gz
   silicon-apps packages commit                 # the accepted package ids
   silicon-apps release commit --version 0.5.0 --package PACKAGE_ID --package PACKAGE_ID
   silicon-apps install 'commit>dev'            # try the development release
   silicon-apps promote commit RELEASE_ID --version 0.5.0
   ```

   Today Silicon Apps validates uploads on its four Linux workers (`linux-x86_64`, `linux-i686`,
   `linux-aarch64`, `linux-armv7hf`); an upload for macOS or Windows is refused until their workers are live.
   Keep those archives from the same artifact and upload them then. A bad release is withdrawn with
   `silicon-apps withdraw commit RELEASE_ID --reason '…'`; updaters move installed copies off it.
5. Publish the crates: `cargo publish -p silicon-commit-client`, then `cargo publish -p silicon-commit-cli`.

The service and the web ship separately: the backend image comes from the **Build pinned ARM64 backend image**
workflow on a `release/**` branch and is rolled out as in [`deploy/aws/README.md`](../deploy/aws/README.md); the web
deploys to its Vercel project.

## Targets

| Silicon Apps target | Rust target | built on |
| --- | --- | --- |
| `linux-x86_64` | `x86_64-unknown-linux-gnu` (glibc 2.28) | `ubuntu-24.04` with cargo-zigbuild |
| `linux-aarch64` | `aarch64-unknown-linux-gnu` (glibc 2.28) | `ubuntu-24.04-arm` with cargo-zigbuild |
| `windows-x86_64` | `x86_64-pc-windows-msvc` | `windows-2025` |
| `windows-aarch64` | `aarch64-pc-windows-msvc` | `windows-11-arm` |
| `macos-x86_64` | `x86_64-apple-darwin` | `macos-15-intel` |
| `macos-aarch64` | `aarch64-apple-darwin` | `macos-15` |

Linux builds link against glibc 2.28 with cargo-zigbuild 0.23.4 and Zig 0.15.2, so they run on every Linux the
Silicons use; `scripts/check_linux_abi.py` reads each binary's version requirements and refuses anything newer.
Windows builds link the C runtime statically (`.cargo/config.toml`), so no separate installer is needed. The
packager also knows `linux-i686`, `linux-armv7hf` and `windows-i686`; adding one is a row in the workflow's matrix.

## What the workflow checks

Every target's binary must answer three commands signed out, in an empty home, because Silicon Apps runs them on
each upload and Silicons rely on them to find their way:

```sh
commit --help                # exit 0 and the help text
commit accounts --json       # exit 0 and one JSON object with "app_id": "commit"
commit login status --json   # exit 0 and {"authenticated": false}
```

Each build job runs them on its own runner (on Linux also in a Debian image with glibc 2.28 and no network) and
records the result for those exact bytes. The packaging job refuses to pack a binary without that record, or one
that answers wrongly where it can run.

## Package by hand

```sh
cargo build --locked --release -p silicon-commit-cli --bin commit
scripts/package-apps.sh 0.5.0 macos-aarch64 target/release/commit
```

`scripts/package-apps.sh VERSION TARGET BINARY` renders [`packaging/apps.yaml.in`](../packaging/apps.yaml.in) for
that one target, stages `apps.yaml` and `bin/commit` (`bin/commit.exe` on Windows), refuses a binary built for
another target, runs the three commands when this machine can run the binary, runs `silicon-apps validate` and
`silicon-apps pack`, checks the archive holds exactly those two files and validates it again, and writes
`dist/apps/commit-VERSION-TARGET.tar.gz` with its `.sha256`. It needs Python 3.11+ and silicon-apps 0.2
(`cargo install --locked silicon-apps-cli --version 0.2.0`). `--discovery require` refuses to pack a binary this
machine cannot run unless `--receipt FILE` names the record of a run on its own machine
(`python3 scripts/package_apps.py discover TARGET BINARY --require --receipt-out FILE`).

On macOS with Zig, cargo-zigbuild, cargo-xwin and LLVM installed, `scripts/build-release-macos.sh` cross-builds the
six targets and packs each; the macOS archives get the three commands run locally, the others at upload.

The **ci** workflow packs the Linux CLI with the three commands required on every change, so a broken package
shows up before a tag.
