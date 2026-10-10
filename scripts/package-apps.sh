#!/usr/bin/env bash
# Package one commit CLI binary for Silicon Apps.
#
#   scripts/package-apps.sh <version> <target> <binary>
#   scripts/package-apps.sh 0.5.0 macos-aarch64 target/release/commit
#
# Writes dist/apps/commit-<version>-<target>.tar.gz (apps.yaml listing only <target>, plus bin/commit or
# bin/commit.exe) and its .sha256, after `silicon-apps validate` and `silicon-apps pack`. It first refuses a binary
# that is not native for the target (a Linux one must run on glibc 2.28) and, when this machine can run it, a binary
# that does not answer `commit --help`, `commit accounts --json` and `commit login status --json` signed out in an
# empty home. `--discovery require` (or PACKAGE_DISCOVERY=require) also refuses when it cannot run here, unless
# `--receipt FILE` names the record of a discovery run on the target's own machine. Publishes nothing.
#
# Needs Python 3.11+ and silicon-apps 0.2 (`cargo install --locked silicon-apps-cli --version 0.2.0`; or set
# SILICON_APPS to its path). `scripts/package-apps.sh --help` lists every option.
set -euo pipefail

here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
for candidate in "${PYTHON:-}" python3 python; do
  [ -n "$candidate" ] || continue
  if command -v "$candidate" >/dev/null 2>&1 &&
    "$candidate" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 11) else 1)' >/dev/null 2>&1; then
    exec "$candidate" "$here/package_apps.py" "$@"
  fi
done
echo "package-apps: Python 3.11 or newer is required (set PYTHON to its path)" >&2
exit 1
