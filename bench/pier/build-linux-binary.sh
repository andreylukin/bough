#!/usr/bin/env bash
# Build a Linux `bough` for Pier's containers, once, on this machine.
#
#   bench/pier/build-linux-binary.sh                 # aarch64: force_build runs on Apple Silicon
#   ARCH=x86_64 bench/pier/build-linux-binary.sh     # the prebuilt amd64 task images
#
# Writes bench/pier/dist/bough-linux-$ARCH, which the Harbor/Pier adapters upload into every trial.
# Bullseye is EOL (2026-08): its packages come from archive.debian.org, unsigned-dates allowed. Built in
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
  bash -c 'set -e; \
           echo "deb http://archive.debian.org/debian bullseye main" > /etc/apt/sources.list; \
           rm -f /etc/apt/sources.list.d/*; \
           apt-get -o Acquire::Check-Valid-Until=false update -qq >/dev/null && apt-get install -y -qq pkg-config libssl-dev >/dev/null; \
           export OPENSSL_STATIC=1 OPENSSL_LIB_DIR="/usr/lib/$(gcc -print-multiarch)" OPENSSL_INCLUDE_DIR=/usr/include; \
           cargo build --release --locked -p bough 2>&1 | tail -3; \
           touch /src/crates/bough-llm/src/lib.rs 2>/dev/null || true; \
           cargo build --release --locked -p bough 2>&1 | tail -1; \
           cp /target/release/bough /out/bough-linux-'"$ARCH"' && strip /out/bough-linux-'"$ARCH"'; \
           ldd /out/bough-linux-'"$ARCH"' | grep -i ssl && { echo "error: OpenSSL is still dynamic" >&2; exit 1; }; \
           /out/bough-linux-'"$ARCH"' --version || true'
ls -la "$OUT/bough-linux-$ARCH"
