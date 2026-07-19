#!/usr/bin/env bash
# Pass iff the shared helper exists and all three modules delegate to it, the
# duplicated split snippet is gone, tests are untouched + green, and the
# canonical behavior holds on inputs where the drifted copies disagreed.
# $1 = final workspace.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_warehouse.py" "$WS/test_warehouse.py"
cd "$WS"

[ -f lib/parsing.py ]
grep -q "def parse_record" lib/parsing.py
grep -q "parse_record" receiving.py
grep -q "parse_record" shipping.py
grep -q "parse_record" audit.py
grep -q "lib" receiving.py
grep -q "lib" shipping.py
grep -q "lib" audit.py
# The duplicated parsing snippet must be gone from the three modules.
! grep -E "split\(.\|.\)" receiving.py shipping.py audit.py

python3 -m unittest -q

# Canonical helper behavior where the drifted copies disagreed.
[ "$(python3 -c "from lib.parsing import parse_record; print(parse_record(' W-9 | 2 | dock A # rush'))")" = "('W-9', 2, 'dock A')" ]
[ "$(python3 -c "from lib.parsing import parse_record; print(parse_record('# note'))")" = "None" ]
[ "$(python3 -c "
from lib.parsing import parse_record
try:
    parse_record('X|1 # oops')
    print('no error')
except ValueError as e:
    print(e)
")" = "bad record: 'X|1 # oops'" ]

# The modules themselves, pinned on divergent inputs.
[ "$(python3 -c "from receiving import receive; print(receive([' W-1 | 2 | dock A', '# closed', 'W-1|3|dock A # rush']))")" = "{'W-1': 5}" ]
[ "$(python3 -c "from shipping import ship_manifest; print(ship_manifest([' W-1 | 2 |  dock A ']))")" = "['W-1 x2 @ dock A']" ]
[ "$(python3 -c "from audit import audit; print(audit(['W-1| 1 | dock A ', 'W-2|2|dock A # rush', '# note']))")" = "{'dock A': 2}" ]
