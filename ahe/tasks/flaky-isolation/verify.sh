#!/usr/bin/env bash
# Grade one attempt. $1 = the workspace the agent worked in.
#
# The bug is order- and repetition-sensitive, so the visible suite is run three
# ways: once, twice in one process, and reversed. A registry with module-level
# state passes the first and fails the others.
set -u
WS="$1"
TASK="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

fail() { echo "FAIL: $*"; exit 1; }

if ! diff -q "$TASK/fixture/regs/api.py" "$WS/regs/api.py" >/dev/null 2>&1; then
  fail "regs/api.py was modified (it is protected)"
fi
if ! diff -q "$TASK/fixture/test_regs.py" "$WS/test_regs.py" >/dev/null 2>&1; then
  fail "test_regs.py was modified (the tests are not the thing to fix)"
fi

if ! (cd "$WS" && python3 -m unittest test_regs -v) >"$WS/.visible.log" 2>&1; then
  fail "the checked-in test_regs.py no longer passes"$'\n'"$(tail -20 "$WS/.visible.log")"
fi

if ! (cd "$WS" && python3 -m unittest test_regs test_regs -v) >"$WS/.twice.log" 2>&1; then
  fail "test_regs.py does not survive running twice in one process"$'\n'"$(tail -20 "$WS/.twice.log")"
fi

if ! (cd "$WS" && python3 -m unittest \
      test_regs.TestRegistry.test_c_by_tag \
      test_regs.TestRegistry.test_b_names_are_sorted \
      test_regs.TestRegistry.test_a_register_and_get -v) >"$WS/.rev.log" 2>&1; then
  fail "test_regs.py does not survive being reordered"$'\n'"$(tail -20 "$WS/.rev.log")"
fi

cp "$TASK/hidden/test_hidden.py" "$WS/test_hidden.py"
(cd "$WS" && python3 -m unittest test_hidden -v) >"$WS/.hidden.log" 2>&1
hidden=$?
rm -f "$WS/test_hidden.py" "$WS"/__pycache__/test_hidden.*
[ $hidden -eq 0 ] || fail "registries are still not isolated"$'\n'"$(tail -30 "$WS/.hidden.log")"
echo "PASS"
