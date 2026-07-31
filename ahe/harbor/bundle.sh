#!/usr/bin/env bash
# Build the bundle the harbor adapter ships into each task container.
#
# One file instead of a file tree plus a network install: uploading src/ and
# running `bun install` per trial cost ~2 minutes and timed out 12 of 89 tasks in
# agent setup, each scored as a zero for a task bough never got to attempt.
#
# Tests are excluded — they are a third of src/ and no trial runs them.
set -euo pipefail
REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
OUT="${BOUGH_BUNDLE:-/tmp/bough-bundle.tgz}"
cd "$REPO"
[ -d node_modules ] || { echo "node_modules is missing — run 'bun install' first" >&2; exit 1; }

# Ship the LINUX bun binary in the bundle. The container otherwise has to fetch it
# at trial time, which needs curl AND unzip — and an image carrying neither fails
# with "unzip is required to install bun", i.e. a task scored zero over a missing
# archiver. Bundling it removes the last network dependency from install().
# The TASK images' architecture, which is not this machine's. Terminal-Bench 2
# publishes linux/amd64, so on an arm64 Mac every task runs emulated and a probe
# like `docker run ubuntu uname -m` answers for the HOST's variant of a
# multi-arch image, not for the images that will actually run. Getting this wrong
# produces "cannot execute: required file not found" — an ELF the loader cannot
# read, which looks like a corrupt binary rather than a wrong architecture.
ARCH="${BOUGH_TARGET_ARCH:-x86_64}"
case "$ARCH" in
  aarch64|arm64) GLIBC=bun-linux-aarch64;     MUSL=bun-linux-aarch64-musl ;;
  x86_64|amd64)  GLIBC=bun-linux-x64;         MUSL=bun-linux-x64-musl ;;
  *) echo "unknown container arch: $ARCH" >&2; exit 1 ;;
esac

# BOTH libc flavours. Terminal-Bench images are a mix — Ubuntu and friends are
# glibc, Alpine is musl — and a glibc binary on musl does not fail with something
# legible, it fails with "cannot execute: required file not found", which is the
# dynamic loader missing and reads like a corrupt download. install() picks by
# probing for the musl loader, so one bundle serves every image.
fetch() {
  local build="$1"
  [ -x "$REPO/.bough-bun/$build/bun" ] && return 0
  echo "fetching $build ..."
  mkdir -p "$REPO/.bough-bun"
  curl -fsSL -o "/tmp/$build.zip" \
    "https://github.com/oven-sh/bun/releases/latest/download/$build.zip"
  unzip -q -o "/tmp/$build.zip" -d "$REPO/.bough-bun"
  chmod +x "$REPO/.bough-bun/$build/bun"
}
fetch "$GLIBC"; fetch "$MUSL"
cp "$REPO/.bough-bun/$GLIBC/bun" "$REPO/.bough-bun-linux"
cp "$REPO/.bough-bun/$MUSL/bun"  "$REPO/.bough-bun-linux-musl"
# --no-mac-metadata/--no-xattrs matter: macOS bsdtar otherwise writes
# LIBARCHIVE.xattr.com.apple.provenance headers that GNU tar inside the container
# does not know, warns about once per file, and EXITS 1 for — which killed the
# install under `set -e` while extracting a perfectly good archive.
COPYFILE_DISABLE=1 tar czf "$OUT" \
  --no-mac-metadata --no-xattrs \
  --exclude='*.test.ts' --exclude='.git' \
  src node_modules package.json bun.lock bunfig.toml tsconfig.json \
  .bough-bun-linux .bough-bun-linux-musl
echo "$OUT ($(du -h "$OUT" | cut -f1))"
