#!/usr/bin/env bash
# Grade one attempt. $1 = the workspace the agent worked in.
#
# Nothing to preserve here — the package did not exist. The hidden suite IS the
# contract, and the prompt states it in full.
set -u
WS="$1"
TASK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() { echo "FAIL: $*"; exit 1; }

[ -d "$WS/dsv" ] || fail "no dsv/ package was created"

cp "$TASK/hidden/test_hidden.py" "$WS/test_hidden.py"
(cd "$WS" && python3 -m unittest test_hidden -v) >"$WS/.hidden.log" 2>&1
hidden=$?
rm -f "$WS/test_hidden.py" "$WS"/__pycache__/test_hidden.*
[ $hidden -eq 0 ] || fail "dsv does not match the spec"$'\n'"$(tail -30 "$WS/.hidden.log")"
echo "PASS"
