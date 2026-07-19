#!/usr/bin/env bash
# Fresh-machine bootstrap in one line — clones the repo and hands off to the full
# setup (deps, worker model, API-key prompt, launchd service):
#
#   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
#
# Clones into $BOUGH_DIR (default ~/bough). Safe to re-run: an existing clone is
# fast-forwarded, and setup itself is idempotent.
set -euo pipefail

DIR="${BOUGH_DIR:-$HOME/bough}"
REPO="https://github.com/andreylukin/bough.git"

if [ "$(uname -s)" != "Darwin" ]; then
  echo "error: bough's sandbox requires macOS (Seatbelt)." >&2
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
