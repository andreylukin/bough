#!/usr/bin/env bash
# Fresh-machine bootstrap in one line — clones the repo and hands off to the full
# setup (deps, API-key prompt, background service):
#
#   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh)"
#
# Clones into $BOUGH_DIR (default ~/bough). Safe to re-run: an existing clone is
# fast-forwarded, and setup itself is idempotent.
#
# WHICH COMMIT YOU GET. The newest release tag, else `main` when there are no
# tags yet. bough publishes no binaries, so installing means building whatever
# this resolves to — and `main` is a branch people push to, which is the wrong
# thing to hand someone who is meeting the project for the first time. A red
# main is a broken install for everyone who arrives during it; a tag is a commit
# that was green when it was named.
#
#   BOUGH_REF=main   the tip, and its risks — what this script used to do always
#   BOUGH_REF=v0.2.0 a specific release
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
  echo "==> $DIR already exists — fetching"
  git -C "$DIR" fetch --tags --force origin
elif [ -e "$DIR" ]; then
  echo "error: $DIR exists and is not a git checkout — set BOUGH_DIR somewhere else" >&2
  exit 1
else
  echo "==> cloning bough into $DIR"
  git clone "$REPO" "$DIR"
fi

# The newest tag by version order, or empty when the repo has none. `--sort` on
# `git tag` is a plain string sort until `v:refname` makes it a version sort,
# which is the difference between v0.10.0 and v0.9.0 being ordered right.
REF="${BOUGH_REF:-}"
if [ -z "$REF" ]; then
  REF="$(git -C "$DIR" tag --list 'v*' --sort=-v:refname | head -n 1)"
fi
if [ -z "$REF" ]; then
  # No tags cut yet. Say so rather than silently installing the tip — what you
  # are running is a moving target, and that is worth one line.
  echo "==> no release tags yet — installing the tip of main"
  REF="main"
fi

echo "==> checking out $REF"
# `--force` because a re-run over a checkout with local edits should land on the
# ref it names; `bough update` is the path that preserves local work (it carries
# uncommitted changes across as a patch), and this is the fresh-install path.
git -C "$DIR" checkout --force --quiet "$REF" 2>/dev/null || {
  echo "error: $REF is not a ref in this clone." >&2
  echo "  available tags: $(git -C "$DIR" tag --list 'v*' --sort=-v:refname | tr '\n' ' ')" >&2
  exit 1
}

exec "$DIR/scripts/bough" setup
