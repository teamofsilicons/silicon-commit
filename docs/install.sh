#!/bin/sh
set -eu
# The release tarball includes the CLI, stateless client and bundled guides.
COMMIT_VERSION=0.2.0
COMMIT_DOCS=https://docs.commit.teamofsilicons.com
COMMIT_HOME=${SILICON_HOME:-$HOME}
COMMIT_ROOT=$COMMIT_HOME/.commit
case $(uname -s) in Darwin|Linux) ;; *) echo 'Commit supports macOS and Linux.' >&2; exit 1;; esac
umask 077
mkdir -p "$COMMIT_ROOT/bin"
COMMIT_TMP=$(mktemp -d)
trap 'rm -rf "$COMMIT_TMP"' EXIT HUP INT TERM
curl -fsSL "$COMMIT_DOCS/releases/commit-$COMMIT_VERSION.tar.gz" -o "$COMMIT_TMP/source.tar.gz"
curl -fsSL "$COMMIT_DOCS/releases/commit-$COMMIT_VERSION.sha256" -o "$COMMIT_TMP/source.sha256"
COMMIT_EXPECTED=$(cut -d ' ' -f 1 "$COMMIT_TMP/source.sha256")
if command -v sha256sum >/dev/null 2>&1; then COMMIT_ACTUAL=$(sha256sum "$COMMIT_TMP/source.tar.gz" | cut -d ' ' -f 1); else COMMIT_ACTUAL=$(shasum -a 256 "$COMMIT_TMP/source.tar.gz" | cut -d ' ' -f 1); fi
[ "$COMMIT_ACTUAL" = "$COMMIT_EXPECTED" ] || { echo 'Release checksum mismatch' >&2; exit 1; }
if ! command -v cargo >/dev/null 2>&1; then
 curl -fsSL https://sh.rustup.rs -o "$COMMIT_TMP/rustup.sh"
 sh "$COMMIT_TMP/rustup.sh" -y --profile minimal
 . "$HOME/.cargo/env"
fi
tar -xzf "$COMMIT_TMP/source.tar.gz" -C "$COMMIT_TMP"
cargo install --locked --path "$COMMIT_TMP/silicon-commit/cli" --root "$COMMIT_ROOT" --force
"$COMMIT_ROOT/bin/commit" daemon install
# Put the installed binary on the conventional user PATH without requiring sudo.
mkdir -p "$COMMIT_HOME/.local/bin"
ln -sf "$COMMIT_ROOT/bin/commit" "$COMMIT_HOME/.local/bin/commit"
printf '\nCommit %s is installed. Add it to this shell:\n  export PATH="%s/.local/bin:$PATH"\nThen run: commit iam --json\n' "$COMMIT_VERSION" "$COMMIT_HOME"
