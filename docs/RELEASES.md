# Package a Commit release

Install and update with `honeycomb install 'tos>commit'`. The stateless Rust client
is a normal Cargo dependency; it never replaces itself at runtime.

## Build the native CLI

Use the same release version in the root, client and CLI Cargo manifests and
`honeycomb.yaml`. Build `silicon-commit-cli` with `cargo build --locked --release
-p silicon-commit-cli --target TARGET` for all six targets:

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

## Validate and pack

```sh
python3 scripts/package-release.py --binaries-dir artifacts --output-dir dist
```

The script checks each executable's OS and architecture, checks the manifest
version, stages exactly the six binaries and root `honeycomb.yaml`, runs
`honeycomb validate`, then `honeycomb pack`, then validates the archive. Missing
or wrong-target binaries fail the release. It produces `commit-VERSION.tar.gz`
and SHA-256 checksums. No credentials, source checkout, or local sessions are
included. Publishing and deployment are separate from packaging.

`docs-site/release.py` forwards to this packager for older automation. Building the
documentation alone does not create or claim a native release.

## CI packaging

Run the **Build native Honeycomb package** workflow on the release commit and
supply the reviewed published Honeycomb CLI version. It builds and smoke-tests
all six native executables, then validates and packs a single archive. The
workflow uploads build artifacts; it does not publish or deploy them. Runner
labels follow [GitHub's hosted runner reference](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).
