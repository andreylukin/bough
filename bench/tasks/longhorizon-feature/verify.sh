#!/usr/bin/env bash
# Long-horizon feature: a multi-step extension of tasks.py (flag parsing +
# data-model change + two new commands + a sort with tie-breaks + error paths).
# Grades the final CLI behavior with a scripted session — never the transcript.
set -uo pipefail
WS="$1"
cd "$WS"
rm -f tasks.json  # clean slate; the agent's store format is its own choice

fail() { echo "verify: $1" >&2; exit 1; }
run() { python3 tasks.py "$@"; }

# --- happy path: add with/without priority, then done + list + top ----------
[ "$(run add a)" = "#1" ] || fail "add default -> #1"
[ "$(run add b -p 1)" = "#2" ] || fail "add -p 1 -> #2"
[ "$(run add c --priority 5)" = "#3" ] || fail "add --priority 5 -> #3"
[ "$(run add d -p 1)" = "#4" ] || fail "add -p 1 -> #4"
[ "$(run done 1)" = "done #1" ] || fail "done 1 -> 'done #1'"

# list: priority asc, ties by id asc; #1 is done ([x]); default-priority a=3.
expected=$'#2 [ ] b\n#4 [ ] d\n#1 [x] a\n#3 [ ] c'
got="$(run list)"
[ "$got" = "$expected" ] || fail "list order/format wrong; got:"$'\n'"$got"

# top: highest-priority PENDING is p1, tie #2<#4 -> #2 -> 'b' (#1 done, excluded)
[ "$(run top)" = "b" ] || fail "top -> 'b'"

# --- error paths: unknown id (exit 1) and out-of-range priority (exit 2) -----
if run done 99 >/dev/null 2>&1; then fail "done 99 should exit non-zero"; fi
[ "$(run done 99 2>&1 >/dev/null)" = "no task #99" ] || fail "done 99 stderr"
run done 99 >/dev/null 2>&1; [ "$?" -eq 1 ] || fail "done 99 exit code != 1"

run add e -p 9 >/dev/null 2>&1; [ "$?" -eq 2 ] || fail "add -p 9 exit code != 2"
# the invalid add must not have created a task
[ "$(run top)" = "b" ] || fail "invalid add leaked a task"

echo "ok" >&2
