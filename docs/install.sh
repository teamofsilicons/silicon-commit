#!/bin/sh
# Install the commit CLI with Silicon Apps, which also keeps it up to date.
#   sh install.sh             the production release
#   sh install.sh --yes       pass options through to `silicon-apps install`
set -eu
if ! command -v silicon-apps >/dev/null 2>&1; then
  echo "Commit installs with Silicon Apps, and silicon-apps is not on PATH." >&2
  echo "Install Silicon Apps first: https://developers.teamofsilicons.com/docs/apps/start/install" >&2
  echo "Then run: silicon-apps install commit" >&2
  exit 1
fi
exec silicon-apps install commit "$@"
