#!/usr/bin/env bash
# model-test: the FizzBee model-based tests (recipe: go/tests/model/README.md).
#
#   scripts/model-test.sh              run everything, in this order:
#     1. the fetch script's own checks
#     2. fizz check of every go/tests/model/specs/*.fizz
#     3. go test ./tests/model/...: trace checker, llm-control, fixtures
#        against their specs, and the fizzbee-mbt runs against a real serve
#     4. the TS path generator's tests
#     5. the Playwright model specs (go/tests/web/specs/model/)
#     6. the history-trace check of the transcripts step 5 left behind
#   scripts/model-test.sh gen <spec>   re-check specs/<spec>.fizz, rewrite its
#                                      graph in testdata/<spec>/ and print the
#                                      walks the tests will derive from it
#
# The fizz tools come from scripts/fizz.sh (pinned, sha256-checked, cached
# outside the repo); the first run downloads them. The Playwright step
# needs `npm ci && npx playwright install chromium` in go/tests/web once.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
model="$root/go/tests/model"
fizz="$("$root/scripts/fizz.sh" path fizz)"
# A fresh dir per run under $TMPDIR, left in place: it holds the transcripts
# the history check read, which is what to look at when it fails.
tmp="$(mktemp -d "${TMPDIR:-/tmp}/bough-model-test.XXXXXX")"

# check <spec name> <out dir>: model-check a copy of the spec (fizz writes
# its AST next to the spec) into <out dir>. fizz exits 0 on a failed
# check, so pass is read off its output.
check() {
  local name="$1" out="$2" log
  mkdir -p "$tmp/src"
  cp "$model/specs/$name.fizz" "$tmp/src/$name.fizz"
  log="$("$fizz" --output-dir "$out" "$tmp/src/$name.fizz" 2>&1)" || true
  if ! grep -q '^PASSED:' <<<"$log"; then
    printf '%s\n' "$log" >&2
    echo "model-test: fizz check of specs/$name.fizz FAILED" >&2
    return 1
  fi
  echo "model-test: specs/$name.fizz: $(grep '^PASSED:' <<<"$log")"
}

if [ "${1:-}" = gen ]; then
  [ $# -eq 2 ] || { echo "usage: scripts/model-test.sh gen <spec>" >&2; exit 2; }
  name="$2"
  check "$name" "$tmp/run"
  mkdir -p "$model/testdata/$name"
  cp "$tmp/run"/nodes_*.pb "$tmp/run"/adjacency_lists_*.pb "$model/testdata/$name/"
  (cd "$root/go/tests/web" && node model/gen.ts "$model/testdata/$name" >/dev/null)
  exit 0
fi
[ $# -eq 0 ] || { echo "usage: scripts/model-test.sh [gen <spec>]" >&2; exit 2; }

"$root/scripts/fizz_test.sh"

for spec in "$model"/specs/*.fizz; do
  name="$(basename "$spec" .fizz)"
  check "$name" "$tmp/check-$name"
done

export FIZZ_BIN="$fizz"
export FIZZBEE_MBT_SERVER="$("$root/scripts/fizz.sh" path mbt-server)"
export FIZZBEE_MBT_BIN="$("$root/scripts/fizz.sh" path mbt-runner)"
# -count=1: the MBT and llm-control tests build and run the bough binary,
# a change to which go test's cache cannot see.
(cd "$root/go" && go test -count=1 -race -parallel 4 -p 4 ./tests/model/...)

# node strips the TS types itself (22.18+); the generator has no deps.
(cd "$root/go/tests/web" && npm run --silent test:model)

(cd "$root/go" && go build -o "$tmp/bough" ./cmd/bough)
(cd "$root/go/tests/web" && BOUGH_BIN="$tmp/bough" MODEL_TRACE_DIR="$tmp/traces" npx playwright test specs/model/)

(cd "$root/go" && MODEL_TRACE_DIR="$tmp/traces" go test -count=1 -run '^TestHistoryTraces$' ./tests/model/mbt/)
