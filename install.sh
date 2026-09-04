#!/bin/sh
# bough installer.
#
#   curl -fsSL https://raw.githubusercontent.com/andreylukin/bough/main/install.sh | sh
#
# Downloads the release binary for this OS and CPU and puts it on your
# PATH. Set BOUGH_VERSION to pin a tag, BOUGH_INSTALL_DIR to choose
# where it lands. POSIX sh on purpose: this runs before bough does, on
# whatever shell the machine has.
set -eu

REPO="andreylukin/bough"
VERSION="${BOUGH_VERSION:-latest}"
INSTALL_DIR="${BOUGH_INSTALL_DIR:-$HOME/.local/bin}"

say() { printf '%s\n' "$*"; }
die() { printf 'bough: %s\n' "$*" >&2; exit 1; }

need() { command -v "$1" >/dev/null 2>&1 || die "need $1 on PATH"; }
need uname
need tar

# curl or wget, whichever the machine has.
if command -v curl >/dev/null 2>&1; then
  fetch() { curl -fsSL "$1"; }
  fetch_to() { curl -fsSL "$1" -o "$2"; }
elif command -v wget >/dev/null 2>&1; then
  fetch() { wget -qO- "$1"; }
  fetch_to() { wget -qO "$2" "$1"; }
else
  die "need curl or wget on PATH"
fi

os=$(uname -s)
case "$os" in
  Darwin) os=darwin ;;
  Linux)  os=linux ;;
  *) die "unsupported OS: $os (bough builds for macOS and Linux; Windows is not supported yet)" ;;
esac

arch=$(uname -m)
case "$arch" in
  x86_64|amd64) arch=amd64 ;;
  arm64|aarch64) arch=arm64 ;;
  *) die "unsupported CPU: $arch" ;;
esac

if [ "$VERSION" = latest ]; then
  # The redirect target of /releases/latest names the tag, which avoids
  # depending on the API (rate-limited without a token).
  VERSION=$(fetch "https://api.github.com/repos/$REPO/releases/latest" |
    sed -n 's/.*"tag_name" *: *"\([^"]*\)".*/\1/p' | head -1)
  [ -n "$VERSION" ] || die "could not determine the latest version; set BOUGH_VERSION"
fi

asset="bough_${VERSION}_${os}_${arch}.tar.gz"
url="https://github.com/$REPO/releases/download/$VERSION/$asset"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading bough $VERSION for $os/$arch"
fetch_to "$url" "$tmp/$asset" || die "download failed: $url"

# Verify against the release's checksums when they are published and a
# sha256 tool exists. A silently corrupt binary is worse than no binary.
if checksums=$(fetch "https://github.com/$REPO/releases/download/$VERSION/checksums.txt" 2>/dev/null); then
  if command -v sha256sum >/dev/null 2>&1; then
    got=$(sha256sum "$tmp/$asset" | cut -d' ' -f1)
  elif command -v shasum >/dev/null 2>&1; then
    got=$(shasum -a 256 "$tmp/$asset" | cut -d' ' -f1)
  else
    got=""
  fi
  if [ -n "$got" ]; then
    want=$(printf '%s\n' "$checksums" | grep " $asset\$" | cut -d' ' -f1)
    [ -z "$want" ] || [ "$got" = "$want" ] || die "checksum mismatch for $asset"
    [ -z "$want" ] || say "checksum ok"
  fi
fi

tar -xzf "$tmp/$asset" -C "$tmp" || die "could not unpack $asset"
[ -f "$tmp/bough" ] || die "archive did not contain a bough binary"

mkdir -p "$INSTALL_DIR"
# Replacing a running binary in place gets the copy SIGKILLed on macOS;
# write beside it and rename, which is atomic.
mv "$tmp/bough" "$INSTALL_DIR/bough.new"
chmod +x "$INSTALL_DIR/bough.new"
mv "$INSTALL_DIR/bough.new" "$INSTALL_DIR/bough"

say "installed $("$INSTALL_DIR/bough" --version 2>/dev/null || echo bough) to $INSTALL_DIR/bough"

case ":$PATH:" in
  *":$INSTALL_DIR:"*) ;;
  *) say ""
     say "$INSTALL_DIR is not on your PATH. Add it:"
     say "    export PATH=\"$INSTALL_DIR:\$PATH\"" ;;
esac

say ""
say "Next: put an API key where bough reads it, then run bough."
say "    mkdir -p ~/.bough"
say "    echo 'ANTHROPIC_API_KEY=sk-ant-...' >> ~/.bough/env"
say "    bough"
