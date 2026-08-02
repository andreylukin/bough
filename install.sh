#!/usr/bin/env bash
# Fresh-machine bootstrap in one line — clones the repo and hands off to the full
# setup (deps, API-key prompt, background service):
#
#   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
#
# Clones into $BOUGH_DIR (default ~/bough). Safe to re-run: an existing clone is
# fast-forwarded, and setup itself is idempotent.
set -euo pipefail

DIR="${BOUGH_DIR:-$HOME/bough}"
REPO="https://github.com/andreylukin/bough.git"

# macOS and Linux. Nothing here is confined on either: bough runs programs as you,
# with your full authority, and says so (spec §2). The only thing that differs is
# which service manager `scripts/bough` installs into — launchd or a systemd user
# unit — and `scripts/setup.sh` resolves that itself.
OS="$(uname -s)"
case "$OS" in
  Darwin | Linux) ;;
  *)
    echo "error: bough supports macOS and Linux; this is $OS." >&2
    exit 1
    ;;
esac

if ! command -v git >/dev/null; then
  if [ "$OS" = "Darwin" ]; then
    echo "error: git not found — install the Xcode Command Line Tools: xcode-select --install" >&2
  else
    echo "error: git not found — install it with your package manager, then re-run." >&2
  fi
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
