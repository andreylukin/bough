#!/usr/bin/env bash
# Fresh-machine bootstrap in one line — clones the repo and hands off to the full
# setup (deps, API-key prompt, launchd service):
#
#   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
#
# Clones into $BOUGH_DIR (default ~/bough). Safe to re-run: an existing clone is
# fast-forwarded, and setup itself is idempotent.
set -euo pipefail

DIR="${BOUGH_DIR:-$HOME/bough}"
REPO="https://github.com/andreylukin/bough.git"

# macOS-only because the service is launchd, not because anything is confined:
# bough runs programs as you, with your full authority, and says so (spec §2).
if [ "$(uname -s)" != "Darwin" ]; then
  echo "error: bough's service manager is launchd, so setup is macOS-only." >&2
  exit 1
fi

if ! command -v git >/dev/null; then
  echo "error: git not found — install the Xcode Command Line Tools: xcode-select --install" >&2
  exit 1
fi

if [ -d "$DIR/.git" ]; then
  echo "==> $DIR already exists — fast-forwarding"
  git -C "$DIR" pull --ff-only
elif [ -e "$DIR" ]; then
  echo "error: $DIR exists and is not a git checkout — set BOUGH_DIR somewhere else" >&2
  exit 1
else
  echo "==> cloning bough into $DIR"
  git clone "$REPO" "$DIR"
fi

exec "$DIR/scripts/bough" setup
