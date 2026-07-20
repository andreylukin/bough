#!/usr/bin/env bash
# Pass iff all SIX independent modules are fixed (suite green) with the tests
# untouched. The HEAVY counterpart to fanout-bugs: each module is ~50-70 lines
# with a non-obvious bug that takes real reading to locate, and the six are
# unrelated domains. Doing all six inline loads the parent context with six
# modules of code + debugging noise (context pollution) — the shape where the
# research says delegation to fresh-context subagents should start to pay off.
# Pass/fail is identical serial-vs-delegated; the difference is in the
# orchestration metrics (bench/orch_metrics.py) — parent-ctx, wall, and whether
# a weak model's later modules degrade under the accumulated context.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_all.py" "$WS/test_all.py"
cd "$WS"
python3 -m unittest -q
