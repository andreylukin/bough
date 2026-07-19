#!/usr/bin/env bash
# Pass iff stats works to spec, is wired via commands/stats.py, and old commands still work.
set -euo pipefail
WS="$1"
cd "$WS"
[ -f commands/stats.py ]
[ "$(python3 app.py stats data.txt)" = "$(printf 'min 1\nmax 10\nmean 5.0')" ]
[ "$(python3 app.py count data.txt)" = "5" ]
[ "$(python3 app.py head data.txt)" = "$(printf '10\n4\n7')" ]
