#!/usr/bin/env bash
# Start Commit on this machine against a local Silicon Accounts stack (idempotent):
#
#   COMMIT_TEST_STACK=/path/to/test-stack.json scripts/dev-accounts.sh [--build] [--fresh]
#
# Migrates the database (default commit_e2e on 127.0.0.1:5460), starts commit-api on 127.0.0.1:4141 and
# commit-worker, points Commit's webhook at Silicon Accounts to the API and proves a test delivery.
# Stop with scripts/dev-accounts-stop.sh. Configuration: python3 scripts/dev_accounts.py --help.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec "${PYTHON:-python3}" -I "$here/dev_accounts.py" up "$@"
