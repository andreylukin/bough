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
# CI runs the same steps as parallel jobs; each is also a subcommand:
#   scripts/model-test.sh check        steps 1-4, without tests/model/mbt
#   scripts/model-test.sh mbt I N      shard I of N of tests/model/mbt, its
#                                      tests bin-packed by go/scripts/mbt-timings.tsv
#   scripts/model-test.sh walks I N    shard I of N of step 5; the transcripts
#                                      go to $MODEL_TRACE_DIR (default under $tmp)
#   scripts/model-test.sh history DIR  step 6 over the transcripts in DIR
# One step alone took 47 minutes on a CI runner; run in sequence they
# were most of a four-hour job.
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
usage() { echo "usage: scripts/model-test.sh [gen <spec> | check | mbt I N | walks I N | history DIR]" >&2; exit 2; }

export FIZZ_BIN="$fizz"
export FIZZBEE_MBT_SERVER="$("$root/scripts/fizz.sh" path mbt-server)"
export FIZZBEE_MBT_BIN="$("$root/scripts/fizz.sh" path mbt-runner)"
# tests/model/mbt skips itself on a bare `go test ./...` (it outlasts the
# default ten-minute timeout); this script is where it is meant to run.
export BOUGH_MODEL_TESTS=1
# -count=1: the MBT and llm-control tests build and run the bough binary,
# a change to which go test's cache cannot see.
# The complete state walks now exceed Go's default ten-minute timeout;
# nightly transition coverage walks many more paths through the same code.
mbt_timeout=90m
if [ "${MODEL_COVER:-}" = transitions ]; then mbt_timeout=4h; fi
gotest() { (cd "$root/go" && go test -count=1 -race -parallel 4 -p 4 -timeout "$mbt_timeout" "$@"); }

check_all() {
  "$root/scripts/fizz_test.sh"
  for spec in "$model"/specs/*.fizz; do
    name="$(basename "$spec" .fizz)"
    check "$name" "$tmp/check-$name"
  done
  gotest $(cd "$root/go" && go list ./tests/model/... | grep -v '/tests/model/mbt$')
  # node strips the TS types itself (22.18+); the generator has no deps.
  (cd "$root/go/tests/web" && npm run --silent test:model)
}

# mbt I N: the package's top-level tests, split the way go/scripts/test-shards.sh
# splits packages, so every shard computes the same assignment on its own.
# The *CatchWrongAdapter / *CatchesWrongAdapter tests check the walks
# themselves (a broken adapter must fail them) over every transition:
# a third of the package's time, for a property that changes only when
# a walk does. A push leaves them to the nightly transitions run.
mbt_shard() {
  local tests run
  tests="$(cd "$root/go" && go test -list '.*' ./tests/model/mbt/ | grep '^Test')"
  if [ "${MODEL_COVER:-}" != transitions ]; then tests="$(grep -v 'WrongAdapter$' <<<"$tests")"; fi
  # TestHistoryTraces replays the browser walks' transcripts: the history step's.
  tests="$(grep -v '^TestHistoryTraces$' <<<"$tests")"
  run="$(TEST_SHARDS_TIMINGS="$root/go/scripts/mbt-timings.tsv" "$root/go/scripts/test-shards.sh" "$1" "$2" <<<"$tests" | paste -sd'|' -)"
  [ -n "$run" ] || { echo "model-test: mbt shard $1/$2 is empty" >&2; return 0; }
  gotest -run "^($run)\$" ./tests/model/mbt/
}

walks() {
  local shard=()
  [ $# -eq 2 ] && shard=(--shard="$1/$2")
  if [ -z "${BOUGH_BIN:-}" ]; then
    (cd "$root/go" && go build -o "$tmp/bough" ./cmd/bough)
    BOUGH_BIN="$tmp/bough"
  fi
  (cd "$root/go/tests/web" && BOUGH_BIN="$BOUGH_BIN" MODEL_TRACE_DIR="$MODEL_TRACE_DIR" npx playwright test specs/model/ ${shard[@]+"${shard[@]}"})
}

history_check() {
  (cd "$root/go" && MODEL_TRACE_DIR="$1" go test -count=1 -run '^TestHistoryTraces$' ./tests/model/mbt/)
}

export MODEL_TRACE_DIR="${MODEL_TRACE_DIR:-$tmp/traces}"
case "${1:-}" in
  "") [ $# -eq 0 ] || usage
      check_all
      gotest ./tests/model/mbt/
      walks
      history_check "$MODEL_TRACE_DIR" ;;
  check) [ $# -eq 1 ] || usage; check_all ;;
  mbt) [ $# -eq 3 ] || usage; mbt_shard "$2" "$3" ;;
  walks) [ $# -eq 3 ] || usage; walks "$2" "$3" ;;
  history) [ $# -eq 2 ] || usage; history_check "$2" ;;
  *) usage ;;
esac
