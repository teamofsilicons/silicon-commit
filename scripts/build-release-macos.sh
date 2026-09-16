#!/usr/bin/env bash
# Build all six Honeycomb payloads on macOS with Rust, Zig and LLVM installed.
set -euo pipefail
cd "$(dirname "$0")/.."

if [[ $(uname -s) != Darwin ]]; then
  echo 'This cross-build script requires macOS and the Apple SDK.' >&2
  exit 1
fi
for tool in cargo rustup zig clang-cl llvm-lib lld-link python3; do
  command -v "$tool" >/dev/null || { echo "Missing build tool on PATH: $tool" >&2; exit 1; }
done
cargo zigbuild --help >/dev/null
cargo xwin --version
honeycomb_cli=${HONEYCOMB:-honeycomb}
command -v "$honeycomb_cli" >/dev/null || { echo "Missing Honeycomb CLI: $honeycomb_cli" >&2; exit 1; }
jobs=${COMMIT_BUILD_JOBS:-4}

cargo build --locked --release -p silicon-commit-cli --bin commit -j "$jobs" \
  --target aarch64-apple-darwin --target x86_64-apple-darwin
cargo zigbuild --locked --release -p silicon-commit-cli --bin commit -j "$jobs" \
  --target x86_64-unknown-linux-gnu.2.28 --target aarch64-unknown-linux-gnu.2.28 \
  --target-dir target/cross-linux
cargo xwin build --locked --release -p silicon-commit-cli --bin commit -j "$jobs" \
  --target x86_64-pc-windows-msvc --target aarch64-pc-windows-msvc \
  --target-dir target/cross-windows

stage_binary() {
  mkdir -p "dist/binaries/$1"
  cp "$2" "dist/binaries/$1/$3"
  chmod 755 "dist/binaries/$1/$3"
}
stage_binary macos-aarch64 target/aarch64-apple-darwin/release/commit commit
stage_binary macos-x86_64 target/x86_64-apple-darwin/release/commit commit
stage_binary linux-aarch64 target/cross-linux/aarch64-unknown-linux-gnu/release/commit commit
stage_binary linux-x86_64 target/cross-linux/x86_64-unknown-linux-gnu/release/commit commit
stage_binary windows-aarch64 target/cross-windows/aarch64-pc-windows-msvc/release/commit.exe commit.exe
stage_binary windows-x86_64 target/cross-windows/x86_64-pc-windows-msvc/release/commit.exe commit.exe
python3 scripts/package-release.py --binaries-dir dist/binaries --honeycomb "$honeycomb_cli"
