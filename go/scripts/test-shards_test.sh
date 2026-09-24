#!/usr/bin/env bash
# Tests for go/scripts/test-shards.sh, against a synthetic timings file so
# they neither run go nor depend on the committed timings.
set -u

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
shards="$here/test-shards.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
fails=0

fail() { echo "FAIL: $*"; fails=$((fails + 1)); }
pass() { echo "ok:   $*"; }

printf 'ex/big\t100\nex/mid\t60\nex/a\t30\nex/b\t30\nex/tiny\t1\nex/gone\t500\n' >"$tmp/timings.tsv"
printf 'ex/big\nex/mid\nex/a\nex/b\nex/tiny\nex/new1\nex/new2\nex/new3\n' >"$tmp/pkgs"

run() { TEST_SHARDS_TIMINGS="$tmp/timings.tsv" "$shards" "$@" <"$tmp/pkgs"; }

# Every listed package lands in exactly one shard; a timed package that
# no longer exists (ex/gone) is not invented.
for i in 1 2 3; do run "$i" 3 >"$tmp/s$i" || fail "shard $i/3 exited $?"; done
if [ "$(cat "$tmp"/s1 "$tmp"/s2 "$tmp"/s3 | sort)" = "$(sort "$tmp/pkgs")" ]; then
  pass "shards partition the package list"
else
  fail "partition: $(cat "$tmp"/s1 "$tmp"/s2 "$tmp"/s3 | tr '\n' ' ')"
fi

# Longest first onto the lightest shard: big alone, mid+tiny, a+b.
if [ "$(sort "$tmp/s1" | grep -c '^ex/big$')" = 1 ] && ! grep -q '^ex/mid$\|^ex/a$\|^ex/b$' "$tmp/s1" \
  && grep -q '^ex/mid$' "$tmp/s2" && grep -q '^ex/a$' "$tmp/s3" && grep -q '^ex/b$' "$tmp/s3"; then
  pass "known packages are bin-packed by time"
else
  fail "packing: s1=[$(tr '\n' ' ' <"$tmp/s1")] s2=[$(tr '\n' ' ' <"$tmp/s2")] s3=[$(tr '\n' ' ' <"$tmp/s3")]"
fi

# Packages missing from the timings go round-robin, one per shard.
if grep -q '^ex/new1$' "$tmp/s1" && grep -q '^ex/new2$' "$tmp/s2" && grep -q '^ex/new3$' "$tmp/s3"; then
  pass "unknown packages go round-robin"
else
  fail "round-robin: s1=[$(tr '\n' ' ' <"$tmp/s1")] s2=[$(tr '\n' ' ' <"$tmp/s2")] s3=[$(tr '\n' ' ' <"$tmp/s3")]"
fi

# No timings file at all: everything round-robin, nothing dropped.
out="$(TEST_SHARDS_TIMINGS="$tmp/none.tsv" "$shards" 2 3 <"$tmp/pkgs")"
if [ "$out" = "$(printf 'ex/mid\nex/tiny\nex/new3')" ]; then
  pass "missing timings fall back to round-robin"
else
  fail "no timings: $(echo "$out" | tr '\n' ' ')"
fi

# Bad arguments: usage, exit 2.
for args in "" "0 3" "4 3" "x 3"; do
  # shellcheck disable=SC2086
  out="$(run $args 2>&1)"; code=$?
  if [ "$code" -eq 2 ] && [[ "$out" == *usage* ]]; then
    pass "args '$args' rejected"
  else
    fail "args '$args': exit=$code out=$out"
  fi
done

# The committed timings cover every package the race shards run, so a
# stale file shows up here instead of as a lopsided shard in CI.
if command -v go >/dev/null; then
  missing="$(cd "$here/.." && go list ./... | grep -v /internal/vtreal | while read -r p; do
    grep -q "^$p	" "$here/test-timings.tsv" || echo "$p"; done)"
  if [ -z "$missing" ]; then
    pass "committed timings cover every package"
  else
    echo "note: untimed packages (round-robin until regenerated): $missing"
  fi
fi

[ "$fails" -eq 0 ] || { echo "$fails failed"; exit 1; }
echo "all passed"
