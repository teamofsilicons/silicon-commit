#!/usr/bin/env bash
# End-to-end scenarios against a local Silicon Accounts stack, with real tokens and the real binaries:
#
#   COMMIT_TEST_STACK=/path/to/test-stack.json SILICON_ACCOUNTS_DIR=/path/to/silicon-accounts \
#     scripts/e2e-accounts.sh [--only 1,2,…] [--keep-running]
#
# Starts scripts/dev-accounts.sh when the API is not running. Details: python3 tests/e2e/accounts_e2e.py --help.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/../tests/e2e/accounts_e2e.py" "$@"
