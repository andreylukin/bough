#!/usr/bin/env bash
# Pass iff old behavior is intact and --top matches the spec (incl. tie-break).
set -euo pipefail
WS="$1"
cd "$WS"
[ "$(python3 wordcount.py sample.txt)" = "3 14" ]
[ "$(python3 wordcount.py --top 2 sample.txt)" = "$(printf 'the 4\nand 1')" ]
[ "$(python3 wordcount.py --top 1 sample.txt)" = "the 4" ]
