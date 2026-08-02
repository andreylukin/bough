#!/usr/bin/env bash
# Grade one attempt. $1 = the workspace the agent worked in.
#
# Outcome-graded: this reads the code, never the transcript. No protected file
# here — every module in kit/ is fair game, and all eight have to be right.
set -u
WS="$1"
TASK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() { echo "FAIL: $*"; exit 1; }

# 1. The checked-in suite still passes — no fixing the hidden cases by breaking
#    the visible ones.
if ! (cd "$WS" && python3 -m unittest test_kit -v) >"$WS/.visible.log" 2>&1; then
  fail "the checked-in test_kit.py no longer passes"$'\n'"$(tail -20 "$WS/.visible.log")"
fi

# 2. The hidden suite, copied in only now. One class per module, so a partial
#    fan-out reads as exactly the modules it missed.
cp "$TASK/hidden/test_hidden.py" "$WS/test_hidden.py"
(cd "$WS" && python3 -m unittest test_hidden -v) >"$WS/.hidden.log" 2>&1
hidden=$?
rm -f "$WS/test_hidden.py" "$WS"/__pycache__/test_hidden.*
[ $hidden -eq 0 ] || fail "some modules are still broken"$'\n'"$(grep -E '^(FAIL|ERROR):' "$WS/.hidden.log" | head -20)"
echo "PASS"
