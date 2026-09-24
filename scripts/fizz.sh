#!/usr/bin/env bash
# Run a pinned FizzBee tool without installing it (no Java, no brew).
#
#   scripts/fizz.sh fizz [flags] <spec.fizz>   model checker (upstream wrapper)
#   scripts/fizz.sh mbt-server [flags]         FizzMo state-graph gRPC server
#   scripts/fizz.sh mbt-runner [flags]         MBT runner the Go library execs
#   scripts/fizz.sh path <fizz|mbt-server|mbt-runner>
#                                              print the binary's path, e.g.
#                                              for FIZZBEE_MBT_BIN
#
# Releases are fetched once into ${XDG_CACHE_HOME:-$HOME/.cache}/bough-fizz/
# and checked against the sha256 values below before anything is unpacked:
# the cache lives outside the repo, so a tampered or truncated file there
# would otherwise run with the user's full authority on every spec check.
# See go/tests/model/GRAPH.md for what the tools write and read.
set -euo pipefail

FIZZ_VERSION=0.5.3
# fizzbee-mbt 0.2.0 is the newest release; it pairs with the Go library
# github.com/fizzbee-io/fizzbee/mbt/lib/go@v0.0.0-20251103175550-9e0bc037e5f4,
# the last lib commit before that release. Later lib commits add a proto
# field (SENTINEL_IGNORE) that this server does not know.
MBT_VERSION=0.2.0

sha_for() {
  case "$1" in
    fizzbee-v0.5.3-macos_arm.tar.gz) echo bc702e3a15a4720509bcf70e38bf95ad1b7586dc69d54848deb1cc9674043a50 ;;
    fizzbee-v0.5.3-linux_x86.tar.gz) echo c29c71708cfd7d4e737b7e1a2c2be9266a4f3c23e1c41de904a29c56c926e067 ;;
    fizzbee-mbt-0.2.0-macos_arm.tar.gz) echo ae9296700d3b22aa67510cb5d3fe9e4f2b2cc2ecaa688cf14168cb5721888102 ;;
    fizzbee-mbt-0.2.0-linux_x86.tar.gz) echo 14228be2ea9773178fc2cf6d6cddc4eb9728d2d6e4fd08c990210f8814169dba ;;
    *) return 1 ;;
  esac
}

usage() {
  echo "usage: scripts/fizz.sh <fizz|mbt-server|mbt-runner> [args...]" >&2
  echo "       scripts/fizz.sh path <fizz|mbt-server|mbt-runner>" >&2
  exit 2
}

[ $# -ge 1 ] || usage
cmd="$1"
shift
if [ "$cmd" = path ]; then
  [ $# -eq 1 ] || usage
  tool="$1"
else
  tool="$cmd"
fi
case "$tool" in
  fizz | mbt-server | mbt-runner) ;;
  *) usage ;;
esac

case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) plat=macos_arm ;;
  Linux/x86_64) plat=linux_x86 ;;
  *)
    echo "fizz.sh: no pinned FizzBee build for $(uname -s)/$(uname -m)" >&2
    exit 1
    ;;
esac

sha256() {
  if command -v sha256sum >/dev/null; then
    sha256sum "$1" | cut -d' ' -f1
  else
    shasum -a 256 "$1" | cut -d' ' -f1
  fi
}

# fetch <release-url-dir> <tarball> <dest-dir>: unpack the tarball's single
# top-level directory to <dest-dir> unless it is already there.
fetch() {
  local base="$1" name="$2" dest="$3"
  [ -d "$dest" ] && return 0
  local want got dl="$root/dl"
  want="$(sha_for "$name")"
  mkdir -p "$dl"
  if [ ! -f "$dl/$name" ]; then
    echo "fizz.sh: downloading $name" >&2
    curl -fsSL -o "$dl/$name.part" "$base/$name"
    mv "$dl/$name.part" "$dl/$name"
  fi
  got="$(sha256 "$dl/$name")"
  if [ "$got" != "$want" ]; then
    rm -f "$dl/$name"
    echo "fizz.sh: sha256 mismatch for $name: got $got, want $want (removed; rerun to fetch again)" >&2
    exit 1
  fi
  # Unpack beside the destination and rename, so an interrupted extract
  # never leaves a half-populated directory that the -d check above trusts.
  local stage
  stage="$(mktemp -d "$root/stage.XXXXXX")"
  tar -xzf "$dl/$name" -C "$stage"
  mv "$stage/${name%.tar.gz}" "$dest"
  rmdir "$stage"
}

root="${XDG_CACHE_HOME:-$HOME/.cache}/bough-fizz/$FIZZ_VERSION"
mkdir -p "$root"
case "$tool" in
  fizz)
    fetch "https://github.com/fizzbee-io/fizzbee/releases/download/v$FIZZ_VERSION" \
      "fizzbee-v$FIZZ_VERSION-$plat.tar.gz" "$root/fizzbee"
    bin="$root/fizzbee/fizz"
    ;;
  mbt-server | mbt-runner)
    fetch "https://github.com/fizzbee-io/fizzbee-mbt-releases/releases/download/v$MBT_VERSION" \
      "fizzbee-mbt-$MBT_VERSION-$plat.tar.gz" "$root/mbt-$MBT_VERSION"
    bin="$root/mbt-$MBT_VERSION/fizzbee-$tool"
    ;;
esac

if [ "$cmd" = path ]; then
  echo "$bin"
  exit 0
fi
exec "$bin" "$@"
