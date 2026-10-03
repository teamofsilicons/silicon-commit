#!/bin/sh
set -eu
if ! command -v honeycomb >/dev/null 2>&1; then
  echo "Install Honeycomb first: https://docs.honeycomb.teamofsilicons.com/" >&2
  exit 1
fi
exec honeycomb install 'commit'
