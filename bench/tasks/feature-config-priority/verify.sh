#!/usr/bin/env bash
# Pass iff layered config resolution matches the spec: each layer's precedence,
# the empty-flag-counts-as-set detail, the exact unknown-key error + exit 2,
# and old default behavior intact. $1 = final workspace.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_linefmt.py" "$WS/test_linefmt.py"

V="$(mktemp -d "${TMPDIR:-/tmp}/bench-verify.XXXXXX")"
trap 'rm -rf "$V"' EXIT
cp -R "$WS/." "$V/"
cd "$V"
unset LINEFMT_WIDTH LINEFMT_PREFIX
rm -f .linefmtrc

# Old behavior intact (defaults, no config sources).
[ "$(python3 cli.py render sample.txt)" = "$(printf 'The quick brown fox jumps over the lazy dog near the river\nbank.\nshort line')" ]
[ "$(python3 cli.py count sample.txt)" = "3" ]
python3 -m unittest -q

# Layer 0: defaults, via the new config command.
[ "$(python3 cli.py config)" = "$(printf 'prefix=\nwidth=60')" ]

# Layer 1: config file overrides defaults.
printf 'width=40\n' > .linefmtrc
[ "$(python3 cli.py config)" = "$(printf 'prefix=\nwidth=40')" ]

# Layer 2: environment overrides config file.
[ "$(LINEFMT_WIDTH=50 python3 cli.py config)" = "$(printf 'prefix=\nwidth=50')" ]

# Layer 3: flag overrides environment (and file).
[ "$(LINEFMT_WIDTH=50 python3 cli.py --width 30 config)" = "$(printf 'prefix=\nwidth=30')" ]

# Subtle detail 1: empty-string flag value counts as set.
printf 'prefix=>>\n' > .linefmtrc
[ "$(LINEFMT_PREFIX='##' python3 cli.py config)" = "$(printf 'prefix=##\nwidth=60')" ]
[ "$(LINEFMT_PREFIX='##' python3 cli.py --prefix '' config)" = "$(printf 'prefix=\nwidth=60')" ]

# Resolved settings actually drive render.
rm -f .linefmtrc
[ "$(python3 cli.py --width 30 --prefix '| ' render sample.txt)" = "$(printf '| The quick brown fox jumps over\n| the lazy dog near the river\n| bank.\n| short line')" ]

# Subtle detail 2: unknown key in .linefmtrc -> exact message on stderr, exit 2.
printf 'colour=red\n' > .linefmtrc
set +e
err="$(python3 cli.py config 2>&1 >/dev/null)"
code=$?
set -e
[ "$code" = "2" ]
[ "$err" = "linefmt: unknown key: colour" ]
set +e
python3 cli.py render sample.txt >/dev/null 2>&1
code=$?
set -e
[ "$code" = "2" ]
