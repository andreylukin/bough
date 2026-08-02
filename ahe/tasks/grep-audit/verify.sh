#!/usr/bin/env bash
# Grade one attempt. $1 = the workspace the agent worked in.
#
# Outcome-graded on the artifact: the report is compared byte for byte, and the
# tree it audited has to be untouched.
set -u
WS="$1"
TASK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() { echo "FAIL: $*"; exit 1; }

if ! diff -r -q "$TASK/fixture/app" "$WS/app" >/dev/null 2>&1; then
  fail "app/ was modified — this task is an audit, not a fix"$'\n'"$(diff -r -q "$TASK/fixture/app" "$WS/app" 2>&1 | head -10)"
fi

[ -f "$WS/audit.txt" ] || fail "audit.txt was not written"

if ! diff -u "$TASK/hidden/expected_audit.txt" "$WS/audit.txt" >"$WS/.audit.diff" 2>&1; then
  fail "audit.txt does not match"$'\n'"$(head -40 "$WS/.audit.diff")"
fi
echo "PASS"
