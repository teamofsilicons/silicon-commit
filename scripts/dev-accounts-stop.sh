#!/usr/bin/env bash
# Stop what scripts/dev-accounts.sh started and point Commit's webhook back where it was:
#
#   COMMIT_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts-stop.sh [--keep-webhook]
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/dev_accounts.py" down "$@"
