#!/usr/bin/env bash
# Build a Linux `bough` for Harbor's containers, once, on this machine.
#
#   bench/harbor/build-linux-binary.sh            # x86_64
#   ARCH=aarch64 bench/harbor/build-linux-binary.sh
#
# Writes bench/harbor/dist/bough-linux, which bough_agent.py uploads into every
# trial container. Building in Docker rather than cross-compiling on the host is
# the point: bough links against the system OpenSSL and SQLite, and a macOS
# cross-build finds neither.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARCH="${ARCH:-x86_64}"
OUT="$ROOT/bench/harbor/dist"
IMAGE="rust:1-bookworm"

case "$ARCH" in
  x86_64) PLATFORM=linux/amd64 ;;
  aarch64 | arm64) PLATFORM=linux/arm64 ;;
  *)
    echo "error: ARCH must be x86_64 or aarch64, got $ARCH" >&2
    exit 1
    ;;
esac

if ! command -v docker >/dev/null; then
  echo "error: docker is required (Harbor needs it anyway)." >&2
  exit 1
fi

mkdir -p "$OUT"

# The cargo caches are named volumes, not bind mounts into the checkout: a
# Linux target/ dropped into the working tree would be picked up by `make
# gates` and by the live server, which builds from this tree.
docker run --rm \
  --platform "$PLATFORM" \
  -v "$ROOT:/src:ro" \
  -v "$OUT:/out" \
  -v bough-harbor-cargo-registry:/usr/local/cargo/registry \
  -v "bough-harbor-target-$ARCH:/target" \
  -e CARGO_TARGET_DIR=/target \
  -w /work \
  "$IMAGE" \
  bash -euo pipefail -c '
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends pkg-config libssl-dev >/dev/null
    cp -a /src/. /work/
    rm -rf /work/target
    cargo build --release -p bough
    install -m 0755 /target/release/bough /out/bough-linux
  '

echo "==> $OUT/bough-linux"
file "$OUT/bough-linux" 2>/dev/null || true
