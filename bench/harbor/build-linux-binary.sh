#!/usr/bin/env bash
# Build a Linux `bough` for Harbor's containers, once, on this machine.
#
#   bench/harbor/build-linux-binary.sh            # x86_64 (what TB 2.0 needs)
#   ARCH=aarch64 bench/harbor/build-linux-binary.sh
#
# Writes bench/harbor/dist/bough-linux-$ARCH, which bough_agent.py uploads into
# every trial container. Building in Docker rather than cross-compiling on the
# host is the point: bough links against the system OpenSSL and SQLite, and a
# macOS cross-build finds neither.
#
# x86_64 is the default because Terminal-Bench 2.0 tasks pin PREBUILT amd64
# images (`docker_image = "alexgshaw/…"` in task.toml). On an Apple Silicon Mac
# those run under emulation, and an aarch64 binary uploaded into them dies with
# "cannot execute: required file not found" — the missing ELF interpreter, not
# a missing file. Match the CONTAINER arch, not the host.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ARCH="${ARCH:-x86_64}"
OUT="$ROOT/bench/harbor/dist"
# Bullseye, not bookworm, and OpenSSL linked STATICALLY. Glibc is forward
# compatible but not backward: a binary built on bookworm wants GLIBC_2.32+
# and libssl.so.3, and Terminal-Bench pins some Debian 11 images (the qemu-*
# tasks), where it cannot even start -- 10 trials lost, with the failure
# surfacing as an unrelated apt error. Building on the OLDEST base we must
# support, with libssl.a folded in, produces one binary that runs on both.
IMAGE="rust:1-bullseye"

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

# The checkout is mounted READ-ONLY and built in place, with CARGO_TARGET_DIR
# pointed at a named volume. Two reasons, both learned the hard way:
#
#   - Copying the tree into the container copies target/, which on a working
#     checkout is tens of gigabytes, and the copy dies with "No space left on
#     device" long before cargo starts.
#   - A Linux target/ written into the checkout would be picked up by `make
#     gates` and by the live server, which builds from this tree.
#
# --locked because a read-only source cannot have its Cargo.lock rewritten: an
# out-of-date lock must fail loudly here rather than mid-build.
docker run --rm \
  --platform "$PLATFORM" \
  -v "$ROOT:/src:ro" \
  -v "$OUT:/out" \
  -v bough-harbor-cargo-registry:/usr/local/cargo/registry \
  -v "bough-harbor-target-$ARCH-bullseye:/target" \
  -e CARGO_TARGET_DIR=/target \
  -e ARCH="$ARCH" \
  "$IMAGE" \
  bash -euo pipefail -c '
    apt-get update -qq
    apt-get install -y -qq --no-install-recommends pkg-config libssl-dev >/dev/null
    # libssl-dev ships libssl.a, so this needs neither a vendored build nor
    # any change to the workspace dependencies. OPENSSL_STATIC alone is NOT
    # enough: openssl-sys finds the library through pkg-config, which hands
    # back the dynamic one and ignores the flag. Naming the directories is
    # what makes it link libssl.a, and a static OpenSSL is the whole point --
    # bullseye has libssl.so.1.1 and bookworm has libssl.so.3, so any dynamic
    # link picks one Debian release and fails on the other.
    export OPENSSL_STATIC=1
    export OPENSSL_LIB_DIR="/usr/lib/$(gcc -print-multiarch)"
    export OPENSSL_INCLUDE_DIR=/usr/include
    cargo build --release --locked --manifest-path /src/Cargo.toml -p bough
    install -m 0755 /target/release/bough "/out/bough-linux-$ARCH"
    # Stripped because this binary is uploaded into every sandbox — 89 tasks
    # times k trials, over the network on Daytona. Symbols cost more there than
    # they are worth; the source build is where you debug.
    strip "/out/bough-linux-$ARCH"
  '

echo "==> $OUT/bough-linux-$ARCH"
file "$OUT/bough-linux-$ARCH" 2>/dev/null || true
