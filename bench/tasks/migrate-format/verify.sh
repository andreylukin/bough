#!/usr/bin/env bash
# Pass iff records.dat is migrated byte-exactly and idempotently, the reader
# still reads v1 (tests untouched + green, legacy.dat intact), the provided
# round-trip checker passes against the workspace's reader/writer, and v2
# reading tolerates unknown keys, any field order, and escaped values.
# $1 = final workspace.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_recio.py" "$WS/test_recio.py"
cmp -s "$HERE/fixture/legacy.dat" "$WS/legacy.dat"
[ -f "$WS/migrate.py" ]

V="$(mktemp -d "${TMPDIR:-/tmp}/bench-verify.XXXXXX")"
trap 'rm -rf "$V"' EXIT
cp -R "$WS/." "$V/"
# The checker and dump harnesses are graded as provided, not as (possibly) edited.
cp "$HERE/fixture/checker.py" "$V/checker.py"
cp "$HERE/fixture/dump.py" "$V/dump.py"
cd "$V"

cat > expected_v2.dat <<'EOF'
#recio v2
id=r1
name=alpha widget
qty=4
note=fragile

id=r2
name=beta
qty=12
note=size\=XL, ship flat

id=r3
name=gamma
qty=1
note=

id=r4
name=back\\slash
qty=2
note=keep \\n literal
EOF

# The checked-in data file was migrated, byte-exactly.
cmp -s records.dat expected_v2.dat
# Idempotent: migrating an already-v2 file changes nothing.
python3 migrate.py records.dat
cmp -s records.dat expected_v2.dat
# A fresh migration from the original v1 bytes yields the same bytes.
cp "$HERE/fixture/records.dat" fresh.dat
python3 migrate.py fresh.dat
cmp -s fresh.dat expected_v2.dat

# v1 reading is intact, and the round-trip acceptance checks pass.
python3 -m unittest -q
python3 -m unittest -q checker

# Old v1 files still load through the reader.
[ "$(python3 dump.py legacy.dat)" = "$(printf "id='a7' name='old unit' qty='9' note='archived'\nid='a8' name='relic' qty='1' note='note=with, commas'")" ]

# v2 read tolerance: unknown keys ignored, any field order, missing field -> "".
cat > future.dat <<'EOF'
#recio v2
id=z9
qty=7
color=blue
name=zeta
EOF
[ "$(python3 dump.py future.dat)" = "id='z9' name='zeta' qty='7' note=''" ]

# Escaped values decode exactly (newline, equals, backslash).
cat > esc.dat <<'EOF'
#recio v2
id=e1
name=two\nlines
qty=1
note=a\=b\\c
EOF
cat > esc.expected <<'EOF'
id='e1' name='two\nlines' qty='1' note='a=b\\c'
EOF
[ "$(python3 dump.py esc.dat)" = "$(cat esc.expected)" ]
