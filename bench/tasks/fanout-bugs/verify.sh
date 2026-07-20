#!/usr/bin/env bash
# Pass iff all four independent modules are fixed (suite green) with the tests
# untouched. Decomposable into 4 independent subtasks, so it REWARDS parallel
# delegation (a subagent per module) — but is solvable serially too. The pass/fail
# is identical either way; the difference lives in the orchestration metrics
# (bench/orch_metrics.py): did the agent fan out, and did it stay lean?
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_all.py" "$WS/test_all.py"
cd "$WS"
python3 -m unittest -q
