#!/usr/bin/env bash
# Build a Linux `bough` for Pier's containers, once, on this machine.
#
#   bench/pier/build-linux-binary.sh                 # aarch64: force_build runs on Apple Silicon
#   ARCH=x86_64 bench/pier/build-linux-binary.sh     # the prebuilt amd64 task images
#
# Writes bench/pier/dist/bough-linux-$ARCH, which bough_agent.py uploads into every trial. Built in
# Docker rather than cross-compiled: bough links against the system OpenSSL (reqwest → native-tls)
# and bundles SQLite. Bullseye, OpenSSL static — a bookworm build wants glibc 2.32+ and libssl.so.3,
# which older task images do not have. Match the CONTAINER's arch, not the host's.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARCH="${ARCH:-aarch64}"
OUT="$ROOT/bench/pier/dist"
IMAGE="rust:1-bullseye"

case "$ARCH" in
  x86_64) PLATFORM=linux/amd64 ;;
  aarch64 | arm64) PLATFORM=linux/arm64; ARCH=aarch64 ;;
  *) echo "error: ARCH must be x86_64 or aarch64, got $ARCH" >&2; exit 1 ;;
esac

mkdir -p "$OUT"
# The checkout is mounted READ-ONLY; CARGO_TARGET_DIR is a named volume so a Linux target/ never
# lands in the checkout (make gates and the live build read this tree).
docker run --rm \
  --platform "$PLATFORM" \
  -v "$ROOT:/src:ro" \
  -v "$OUT:/out" \
  -v "bough-pier-target-$ARCH:/target" \
  -v "bough-pier-cargo-registry:/usr/local/cargo/registry" \
  -e CARGO_TARGET_DIR=/target \
  -e OPENSSL_STATIC=1 \
  -w /src \
  "$IMAGE" \
  bash -c 'set -e; apt-get update -qq >/dev/null && apt-get install -y -qq pkg-config libssl-dev >/dev/null; \
           cargo build --release --locked -p bough 2>&1 | tail -3; \
           cp /target/release/bough /out/bough-linux-'"$ARCH"'; \
           /out/bough-linux-'"$ARCH"' --version || true'
ls -la "$OUT/bough-linux-$ARCH"
