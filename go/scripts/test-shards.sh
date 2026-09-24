#!/usr/bin/env bash
# Splits the package list on stdin into TOTAL shards and prints shard INDEX
# (1-based), one package per line:
#
#   go list ./... | grep -v /internal/vtreal | scripts/test-shards.sh 2 4
#
# Round-robin by name put plugins/ui, plugins/tools and tests/model/mbt
# (together over half the race run) into the same shard often enough that
# the slowest shard was most of the unsharded time. So packages with a
# recorded time go longest-first onto the currently lightest shard, and a
# package the timings do not know yet (new since the file was written)
# goes round-robin: a lopsided shard until the file is regenerated, never
# a package that no shard runs.
#
# The timings file is `import-path<TAB>seconds`, from a race run:
#
#   go test -race -parallel 4 -p 4 -count=1 -json $(go list ./... | grep -v /internal/vtreal) |
#     jq -r 'select(.Test==null and .Elapsed!=null and (.Action=="pass" or .Action=="fail" or .Action=="skip"))
#            | "\(.Package)\t\(.Elapsed)"' | sort >scripts/test-timings.tsv
#
# Every shard computes the whole assignment from the same inputs and
# prints its own part, so the shards agree without talking to each other.
set -euo pipefail

usage() { echo "usage: $0 INDEX TOTAL <packages  (1 <= INDEX <= TOTAL)" >&2; exit 2; }
[ $# -eq 2 ] || usage
index=$1 total=$2
[[ "$index" =~ ^[0-9]+$ && "$total" =~ ^[0-9]+$ ]] || usage
[ "$total" -ge 1 ] && [ "$index" -ge 1 ] && [ "$index" -le "$total" ] || usage

timings="${TEST_SHARDS_TIMINGS:-$(dirname "${BASH_SOURCE[0]}")/test-timings.tsv}"
[ -f "$timings" ] || timings=/dev/null

# Known packages sort first, slowest first (negated seconds), ties by path
# so the order is the same on every runner; unknown ones follow in input
# order.
awk -F'\t' -v tf="$timings" 'FILENAME == tf { if ($0 !~ /^#/ && NF >= 2) t[$1] = $2; next }
            NF { if ($1 in t) print 0 "\t" (0 - t[$1]) "\t" $1; else print 1 "\t" ++n "\t" $1 }' "$timings" - |
  sort -t "$(printf '\t')" -k1,1n -k2,2g -k3,3 |
  awk -F'\t' -v idx="$index" -v total="$total" '
    BEGIN { for (s = 1; s <= total; s++) load[s] = 0 }
    $1 == 0 {
      best = 1
      for (s = 2; s <= total; s++) if (load[s] < load[best]) best = s
      load[best] -= $2
      if (best == idx) print $3
      next
    }
    { if (rr++ % total + 1 == idx) print $3 }'
