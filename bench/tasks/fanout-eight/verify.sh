#!/usr/bin/env bash
# Pass iff all EIGHT independent modules are fixed (suite green) with the tests
# untouched. The heaviest fan-out in the bank: 8 unrelated modules (~230 lines
# total) each with a non-obvious bug that takes real reading to locate. Doing
# all eight inline loads the parent context with eight modules of code + eight
# rounds of debugging noise — heavier than fanout-heavy (6), by design pushed
# past the point where a weak model's later fixes degrade under the accumulated
# context. That is the shape where delegating each module to a fresh-context
# subagent should start to move PASS RATE, not just parent-ctx.
#
# Pass/fail is identical serial-vs-delegated when the model doesn't degrade;
# grade the *how* with bench/orch_metrics.py (delegated, parallel, parent-ctx).
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_all.py" "$WS/test_all.py"
cd "$WS"
python3 -m unittest -q
