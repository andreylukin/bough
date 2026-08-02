#!/usr/bin/env bash
# Grade one attempt. $1 = the workspace the agent worked in.
#
# Outcome-graded. The interesting failure here is a blind textual sweep, so the
# hidden suite checks the decoys as hard as it checks the rename.
set -u
WS="$1"
TASK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() { echo "FAIL: $*"; exit 1; }

if ! (cd "$WS" && python3 -m unittest test_svc -v) >"$WS/.visible.log" 2>&1; then
  fail "the checked-in test_svc.py no longer passes"$'\n'"$(tail -20 "$WS/.visible.log")"
fi

cp "$TASK/hidden/test_hidden.py" "$WS/test_hidden.py"
(cd "$WS" && python3 -m unittest test_hidden -v) >"$WS/.hidden.log" 2>&1
hidden=$?
rm -f "$WS/test_hidden.py" "$WS"/__pycache__/test_hidden.*
[ $hidden -eq 0 ] || fail "the rename is wrong"$'\n'"$(tail -30 "$WS/.hidden.log")"

# dns.py is a decoy in its entirety: a sweep that touched it at all is a fail,
# even if the suite happened not to notice.
if ! diff -q "$TASK/fixture/svc/dns.py" "$WS/svc/dns.py" >/dev/null 2>&1; then
  fail "svc/dns.py was modified — NameResolver.resolve is a different method"
fi
echo "PASS"
