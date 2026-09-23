# Package a Commit release

Install and update with `honeycomb install 'commit'`. The stateless Rust client
is a normal Cargo dependency; it never replaces itself at runtime.

## IAM 2 recovery rollout

The IAM 2 adapter and migration `0029_public_membership_ids.sql` must roll out
together. Drain the old API and worker processes before running the privileged
`commit-migrate` binary, then start the new API and workers. The old backend
expects UUID membership values and is incompatible with the migrated text
column. Rolling back its binary alone is not supported.

Migration 0029 backfills every production and testing membership as
`actor_id[org_id]`, retaining the organization/principal keys, work history,
permissions and idempotency records. Migration 0030 repairs orphaned task
descendants and their linked todos, records the repair in project history, and
avoids sending historical repair emails.

Verify the deployed IAM application credentials and audience before reopening
traffic. The audience defaults to `COMMIT_IAM_APP_ID` unless
`COMMIT_IAM_AUDIENCE` is explicitly configured. An `invalid_client` response is
a server configuration failure; asking users to log in again cannot repair it.
Preserve the existing testing-environment encryption key during any credential
rotation.

Release the CLI alongside the backend to deliver session refresh, organization
discovery and local report backups. After rollout, verify login, an ordinary
command without `--org-id`, assigned todo creation, project/task creation,
linked-todo deletion and a retained pre-upgrade record. Delegated requests can
resolve their verified caller for self-assignment; additional assignees require
IAM delegated-directory support and currently return 403.

## Build the native CLI

Use the same release version in the root, client and CLI Cargo manifests and
`honeycomb.yaml`. Build Windows and macOS with `cargo build --locked --release
-p silicon-commit-cli --target TARGET`. Linux releases support glibc 2.28 and
newer; use cargo-zigbuild 0.23.4 with Zig 0.15.2 and an explicit baseline:

```sh
cargo zigbuild --locked --release -p silicon-commit-cli \
  --target x86_64-unknown-linux-gnu.2.28 \
  --target aarch64-unknown-linux-gnu.2.28
```

The six release targets are:

| Honeycomb target | Rust target |
| --- | --- |
| linux-x86_64 | x86_64-unknown-linux-gnu |
| linux-aarch64 | aarch64-unknown-linux-gnu |
| windows-x86_64 | x86_64-pc-windows-msvc |
| windows-aarch64 | aarch64-pc-windows-msvc |
| macos-x86_64 | x86_64-apple-darwin |
| macos-aarch64 | aarch64-apple-darwin |

Use native build hosts or configured cross-compilation toolchains. Run the CLI
smoke checks on each supported host. The packaging command accepts local Cargo
outputs under `target/<triple>/release` or a gathered artifact directory with
`<honeycomb-target>/commit` (`commit.exe` for Windows).

## Cross-build all six on macOS

With all six Rust targets installed, put `cargo-zigbuild`, `cargo-xwin`, Zig,
LLVM (`clang-cl`, `llvm-lib`, `lld-link`) and Honeycomb on PATH, then run:

```sh
scripts/build-release-macos.sh
# Or select a local packager:
HONEYCOMB=/path/to/honeycomb scripts/build-release-macos.sh
```

This builds both macOS targets with the Apple SDK, Linux targets with a glibc
2.28 baseline, and Windows MSVC targets with the Windows SDK managed by
cargo-xwin. Windows builds statically link the Visual C++ runtime, so no separate
redistributable installer is required. It gathers the executables in
`dist/binaries/` and validates and packs `dist/commit-VERSION.tar.gz`. Set `COMMIT_BUILD_JOBS` to limit concurrency
(default 4). Cross-compilation checks must be followed by runtime smoke checks;
the CI workflow below runs each executable on its corresponding OS and CPU.

## Validate and pack

```sh
python3 scripts/package-release.py --binaries-dir artifacts --output-dir dist
```

The script checks each executable's OS and architecture and reads Linux ELF
version requirements to reject any glibc dependency above 2.28. Missing or invalid
version metadata also fails validation. It checks the manifest version, stages
exactly the six binaries and root `honeycomb.yaml`, runs
`honeycomb validate`, then `honeycomb pack`, then validates the archive. Missing
or wrong-target binaries fail the release. It produces `commit-VERSION.tar.gz`
and SHA-256 checksums. No credentials, source checkout, or local sessions are
included. Publishing and deployment are separate from packaging.

`docs-site/release.py` forwards to this packager for older automation. Building the
documentation alone does not create or claim a native release.

## CI packaging

Run the **Build native Honeycomb package** workflow on the release commit and
supply the reviewed published Honeycomb CLI version. It builds and smoke-tests
all six native executables. Linux uses the pinned Zig toolchain and explicit
glibc 2.28 targets; each Linux binary must pass ELF validation and execute
`--version`, `--help`, and `daemon status` in the pinned Debian 10 image with
glibc 2.28 on its native CPU. These containers run only in remote CI. The workflow
then validates and packs a single archive. The
workflow uploads build artifacts; it does not publish or deploy them. Runner
labels follow [GitHub's hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
