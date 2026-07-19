#!/usr/bin/env bash
# Pass iff channel threads end to end, the discount stage matches the spec
# (incl. the conditional DISCOUNT column and exact alignment on both data
# files), existing stage tests are untouched + green, and the agent-written
# discount test catches a boundary mutant and a rounding mutant.
# $1 = final workspace.
set -euo pipefail
WS="$1"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cmp -s "$HERE/fixture/test_pipeline.py" "$WS/test_pipeline.py"
cmp -s "$HERE/fixture/sales_a.txt" "$WS/sales_a.txt"
cmp -s "$HERE/fixture/sales_b.txt" "$WS/sales_b.txt"
[ -f "$WS/pipeline/discount.py" ]
[ -f "$WS/test_discount.py" ]

V="$(mktemp -d "${TMPDIR:-/tmp}/bench-verify.XXXXXX")"
trap 'rm -rf "$V"' EXIT
cp -R "$WS/." "$V/"
cd "$V"

python3 -m unittest -q

# Exact report output, incl. the DISCOUNT column appearing only on file A.
[ "$(python3 report.py sales_a.txt)" = "$(printf 'REGION  UNITS  REVENUE  DISCOUNT\neast       22    11700      1300\nwest        5     9500         0\nTOTAL      27    21200      1300')" ]
[ "$(python3 report.py sales_b.txt)" = "$(printf 'REGION  UNITS  REVENUE\nnorth      16    15200\nsouth       4     1000\nTOTAL      20    16200')" ]

# Mutant A: discount threshold off by one (qty > 10 instead of >= 10).
cat > pipeline/discount.py <<'EOF'
def apply_discounts(records):
    out = []
    for rec in records:
        rec = dict(rec)
        if rec["channel"] == "web" and rec["qty"] > 10:
            rec["discount"] = rec["revenue"] // 10
            rec["revenue"] -= rec["discount"]
        else:
            rec["discount"] = 0
        out.append(rec)
    return out
EOF
if python3 -m unittest -q 2>/dev/null; then exit 1; fi

# Mutant B: rounds the discount instead of floor division.
cat > pipeline/discount.py <<'EOF'
def apply_discounts(records):
    out = []
    for rec in records:
        rec = dict(rec)
        if rec["channel"] == "web" and rec["qty"] >= 10:
            rec["discount"] = round(rec["revenue"] * 0.1)
            rec["revenue"] -= rec["discount"]
        else:
            rec["discount"] = 0
        out.append(rec)
    return out
EOF
if python3 -m unittest -q 2>/dev/null; then exit 1; fi

exit 0
