#!/usr/bin/env bash
# Grade one attempt. $1 = the workspace the agent worked in.
#
# Twenty near-identical edits: the failure mode is stopping at nineteen, so the
# hidden suite names every module it still finds on the old return type.
set -u
WS="$1"
TASK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() { echo "FAIL: $*"; exit 1; }

if ! diff -q "$TASK/fixture/svc/result.py" "$WS/svc/result.py" >/dev/null 2>&1; then
  fail "svc/result.py was modified (it is already correct)"
fi

if ! (cd "$WS" && python3 -m unittest test_ops -v) >"$WS/.visible.log" 2>&1; then
  fail "the checked-in test_ops.py no longer passes"$'\n'"$(tail -20 "$WS/.visible.log")"
fi

# The literal tuple form must be gone from every op module.
if grep -rn -e '(True,' -e '(False,' "$WS/svc/ops" >"$WS/.legacy.log" 2>&1; then
  fail "legacy tuple returns remain"$'\n'"$(head -20 "$WS/.legacy.log")"
fi

cp "$TASK/hidden/test_hidden.py" "$WS/test_hidden.py"
(cd "$WS" && python3 -m unittest test_hidden -v) >"$WS/.hidden.log" 2>&1
hidden=$?
rm -f "$WS/test_hidden.py" "$WS"/__pycache__/test_hidden.*
[ $hidden -eq 0 ] || fail "the migration is incomplete or changed behaviour"$'\n'"$(tail -30 "$WS/.hidden.log")"
echo "PASS"
