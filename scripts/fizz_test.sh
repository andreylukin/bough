#!/usr/bin/env bash
# Tests for scripts/fizz.sh. Offline by default: every case runs against a
# fresh XDG_CACHE_HOME under a temp dir, and the checksum case pre-seeds the
# cache with a bogus tarball so the script never reaches the network.
# FIZZ_TEST_ONLINE=1 adds the end-to-end case, which downloads the pinned
# release (~40 MB) and model-checks go/tests/model/example/Light.fizz.
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
fizz="$here/fizz.sh"
repo="$(dirname "$here")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fails=0

fail() { echo "FAIL: $*"; fails=$((fails + 1)); }
pass() { echo "ok:   $*"; }

# No tool named: usage on stderr, exit 2, nothing fetched.
out="$(XDG_CACHE_HOME="$tmp/c0" "$fizz" 2>&1)"; code=$?
if [ "$code" -eq 2 ] && [[ "$out" == *usage* ]] && [ ! -e "$tmp/c0/bough-fizz" ]; then
  pass "no args prints usage and exits 2"
else
  fail "no args: exit=$code out=$out"
fi

# Unknown tool: exit 2, nothing fetched.
out="$(XDG_CACHE_HOME="$tmp/c1" "$fizz" nope 2>&1)"; code=$?
if [ "$code" -eq 2 ] && [ ! -e "$tmp/c1/bough-fizz" ]; then
  pass "unknown tool exits 2"
else
  fail "unknown tool: exit=$code out=$out"
fi

# A cached tarball whose sha256 does not match the pin is refused and
# removed, so a truncated or tampered download never gets executed.
case "$(uname -s)/$(uname -m)" in
  Darwin/arm64) plat=macos_arm ;;
  Linux/x86_64) plat=linux_x86 ;;
  *) plat="" ;;
esac
if [ -n "$plat" ]; then
  ver="$(sed -n 's/^FIZZ_VERSION=//p' "$fizz")"
  dl="$tmp/c2/bough-fizz/$ver/dl"
  mkdir -p "$dl"
  echo bogus >"$dl/fizzbee-v$ver-$plat.tar.gz"
  out="$(XDG_CACHE_HOME="$tmp/c2" "$fizz" fizz --help 2>&1)"; code=$?
  if [ "$code" -ne 0 ] && [[ "$out" == *"sha256 mismatch"* ]] && [ ! -e "$dl/fizzbee-v$ver-$plat.tar.gz" ]; then
    pass "bad checksum refused and removed"
  else
    fail "bad checksum: exit=$code out=$out"
  fi
else
  echo "skip: checksum case (unsupported platform $(uname -s)/$(uname -m))"
fi

if [ "${FIZZ_TEST_ONLINE:-}" = 1 ]; then
  work="$tmp/spec"
  mkdir -p "$work"
  cp "$repo/go/tests/model/example/Light.fizz" "$work/"
  out="$(XDG_CACHE_HOME="$tmp/c3" "$fizz" fizz --output-dir "$work/out" "$work/Light.fizz" 2>&1)"; code=$?
  if [ "$code" -eq 0 ] && [[ "$out" == *"PASSED"* ]] && [[ "$out" == *"Unique states: 3"* ]]; then
    pass "fizz model-checks Light.fizz"
  else
    fail "fizz online: exit=$code out=$out"
  fi
  out="$(XDG_CACHE_HOME="$tmp/c3" "$fizz" mbt-server -version 2>&1)"; code=$?
  if [ "$code" -eq 0 ] && [[ "$out" == *"version: 0.2.0"* ]]; then
    pass "mbt-server runs"
  else
    fail "mbt-server online: exit=$code out=$out"
  fi
  p="$(XDG_CACHE_HOME="$tmp/c3" "$fizz" path mbt-runner)"
  if [ -x "$p" ]; then pass "path mbt-runner is executable"; else fail "path mbt-runner: $p"; fi
fi

if [ "$fails" -ne 0 ]; then
  echo "FAIL ($fails)"
  exit 1
fi
echo PASS
