#!/usr/bin/env bash
# Pass iff the tests are untouched and green. $1 = final workspace.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_inventory.py" "$WS/test_inventory.py"
cd "$WS"
python3 -m unittest -q
