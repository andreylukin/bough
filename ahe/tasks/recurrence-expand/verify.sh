#!/usr/bin/env bash
# Grade one attempt. $1 = the workspace the agent worked in.
#
# Outcome-graded: this reads the code, never the transcript.
set -u
WS="$1"
TASK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() { echo "FAIL: $*"; exit 1; }

# 1. The protected file is untouched. Compared to the pristine fixture, so a
#    "harmless" reformat still counts: the constraint was stated, not implied.
if ! diff -q "$TASK/fixture/rrule/api.py" "$WS/rrule/api.py" >/dev/null 2>&1; then
  fail "rrule/api.py was modified (it is protected)"
fi

# 2. The checked-in suite still passes — no fixing the hidden cases by breaking
#    the visible ones.
if ! (cd "$WS" && python3 -m unittest test_rrule -v) >"$WS/.visible.log" 2>&1; then
  fail "the checked-in test_rrule.py no longer passes"$'\n'"$(tail -20 "$WS/.visible.log")"
fi

# 3. The hidden suite, copied in only now. Never in the workspace, so it cannot be
#    read, imported, or accidentally satisfied by editing the tests.
cp "$TASK/hidden/test_hidden.py" "$WS/test_hidden.py"
(cd "$WS" && python3 -m unittest test_hidden -v) >"$WS/.hidden.log" 2>&1
hidden=$?
rm -f "$WS/test_hidden.py" "$WS"/__pycache__/test_hidden.*
[ $hidden -eq 0 ] || fail "the spec is still violated"$'\n'"$(tail -30 "$WS/.hidden.log")"
echo "PASS"
