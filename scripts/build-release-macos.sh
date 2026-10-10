#!/usr/bin/env bash
# Cross-build the six release targets on macOS and pack one Silicon Apps archive per target.
#
# Needs Rust 1.98 with the six targets, cargo-zigbuild 0.23.4 with Zig 0.15.2 (Linux, glibc 2.28), cargo-xwin and
# LLVM (clang-cl, llvm-lib, lld-link) for Windows, Python 3.11+ and silicon-apps 0.2. The macOS archives get the
# discovery commands run here; Linux and Windows archives are checked by the release workflow on their own runners
# and by the Silicon Apps validation worker at upload. Writes dist/apps/; publishes nothing.
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
version=$(python3 scripts/package_apps.py version)
jobs=${COMMIT_BUILD_JOBS:-4}
target_dir=${CARGO_TARGET_DIR:-target}

cargo build --locked --release -p silicon-commit-cli --bin commit -j "$jobs" \
  --target aarch64-apple-darwin --target x86_64-apple-darwin
cargo zigbuild --locked --release -p silicon-commit-cli --bin commit -j "$jobs" \
  --target x86_64-unknown-linux-gnu.2.28 --target aarch64-unknown-linux-gnu.2.28 \
  --target-dir "$target_dir/cross-linux"
cargo xwin build --locked --release -p silicon-commit-cli --bin commit -j "$jobs" \
  --target x86_64-pc-windows-msvc --target aarch64-pc-windows-msvc \
  --target-dir "$target_dir/cross-windows"

scripts/package-apps.sh "$version" macos-aarch64 "$target_dir/aarch64-apple-darwin/release/commit"
scripts/package-apps.sh "$version" macos-x86_64 "$target_dir/x86_64-apple-darwin/release/commit"
scripts/package-apps.sh "$version" linux-x86_64 "$target_dir/cross-linux/x86_64-unknown-linux-gnu/release/commit"
scripts/package-apps.sh "$version" linux-aarch64 "$target_dir/cross-linux/aarch64-unknown-linux-gnu/release/commit"
scripts/package-apps.sh "$version" windows-x86_64 "$target_dir/cross-windows/x86_64-pc-windows-msvc/release/commit.exe"
scripts/package-apps.sh "$version" windows-aarch64 "$target_dir/cross-windows/aarch64-pc-windows-msvc/release/commit.exe"
python3 scripts/package_apps.py checksums dist/apps --version "$version" \
  --expect linux-x86_64 linux-aarch64 windows-x86_64 windows-aarch64 macos-x86_64 macos-aarch64
