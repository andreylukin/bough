#!/usr/bin/env bash
# Harness bench orchestrator: run every task N times through each harness,
# append one JSON line per trial to results/results.jsonl, then print the report.
#
# usage: bench/run.sh [-n TRIALS] [-H cc|bough] [task ...]   (default: 2 trials, all tasks, both)
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TRIALS=2
HARNESSES=(cc bough)
while getopts "n:H:" opt; do
  case "$opt" in
    n) TRIALS="$OPTARG" ;;
    H) HARNESSES=("$OPTARG") ;;
    *) echo "usage: run.sh [-n TRIALS] [-H cc|bough] [task ...]" >&2; exit 2 ;;
  esac
done
shift $((OPTIND - 1))

TASKS=("$@")
[ ${#TASKS[@]} -gt 0 ] || TASKS=($(ls "$BENCH/tasks"))

# Default: restart, because a server left over from a previous sweep runs stale
# CODE, which silently invalidates any harness-edit verification (burned twice).
# BENCH_KEEP_SERVER=1 opts out — reuse a running server (start it if down). Safe
# ONLY when the source is unchanged and prompt variation rides per-session
# --prompt-dir (run-bough.sh): the prompt tuner starts one fresh server per
# campaign and keeps it, so variants no longer pay a restart each.
if [ "${BENCH_KEEP_SERVER:-}" = "1" ]; then
  "$BENCH/server.sh" start >&2   # idempotent: start if down, else reuse
else
  "$BENCH/server.sh" stop >&2 || true
  "$BENCH/server.sh" start >&2
fi
mkdir -p "$RESULTS"
OUT="${BENCH_RESULTS_FILE:-$RESULTS/results.jsonl}"

for task in "${TASKS[@]}"; do
  for trial in $(seq 1 "$TRIALS"); do
    for harness in "${HARNESSES[@]}"; do
      echo "── $task trial $trial [$harness]" >&2
      if line="$("$BENCH/run-$harness.sh" "$task" "$trial")"; then
        echo "$line" >>"$OUT"
        echo "$line" | python3 -c 'import json,sys; r=json.load(sys.stdin); print("   " + ("PASS" if r["pass"] else "fail") + "  %.0fs  $%.4f  status=%s" % (r["wall_ms"]/1000, r.get("cost_usd") or 0, r["status"]))' >&2
      else
        echo "   runner error (exit $?)" >&2
      fi
    done
  done
done

python3 "$BENCH/report.py" "$OUT"
