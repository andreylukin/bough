#!/usr/bin/env bash
# Pass iff the tests are untouched and green, AND the symptom site (report.py,
# which carries a misleading FIXME) is untouched — the fix must land at the
# root cause. $1 = final workspace.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_report.py" "$WS/test_report.py"
cmp -s "$HERE/fixture/report.py" "$WS/report.py"
cd "$WS"
python3 -m unittest -q
