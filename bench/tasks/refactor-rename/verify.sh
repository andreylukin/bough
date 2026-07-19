#!/usr/bin/env bash
# Pass iff the rename is complete (no `calc` word remains), tests untouched + green.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_pricing.py" "$WS/test_pricing.py"
cd "$WS"
! grep -rnw "calc" --include='*.py' .
grep -q "def compute_total" pricing.py
python3 -m unittest -q
