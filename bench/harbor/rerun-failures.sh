#!/usr/bin/env bash
# Re-run only the tasks a previous job did not solve.
#
#   bench/harbor/rerun-failures.sh <previous-job-dir> [pass-number]
#   DRY_RUN=1 bench/harbor/rerun-failures.sh <previous-job-dir>   # just list them
#
# This script LAUNCHES A RUN. Its last line is `exec harbor run`, so there is no
# such thing as running it "just to see the list" -- use DRY_RUN=1 for that.
#
# A failed task is not the same as a task the agent cannot do — at one trial
# each, a suite total carries a lot of coin-flip. This runs a second (or third)
# pass over just the zero-reward tasks, so the extra spend goes where the
# uncertainty is instead of re-solving what already works.
#
# WHAT THE RESULT MEANS: combining passes best-of-N is NOT pass@1 and is not
# comparable to a leaderboard number, which fixes k trials for EVERY task and
# reports the mean. Best-of-N only ever moves a score up, because a task gets
# extra chances precisely when it failed. Report it as "best of N passes",
# alongside the honest first-pass rate, or the number is a lie by construction.
set -euo pipefail

PREV="${1:?usage: rerun-failures.sh <previous-job-dir> [pass-number]}"
PASS="${2:-2}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
JOBS="$(dirname "$PREV")"
NAME="$(basename "$PREV")-pass$PASS"

if [ ! -f "$PREV/result.json" ]; then
  echo "error: no result.json in $PREV — has the job finished?" >&2
  exit 1
fi

# A task is "unsolved" if it scored 0 OR errored out without a reward at all.
# Read into an array the long way: macOS ships bash 3.2, which has no mapfile.
FAILED=()
while IFS= read -r line; do
  [ -n "$line" ] && FAILED+=("$line")
done < <(python3 - "$PREV" <<'PY'
import glob, json, os, sys
prev = sys.argv[1]
for f in sorted(glob.glob(f"{prev}/*/result.json")):
    t = json.load(open(f))
    reward = ((t.get("verifier_result") or {}).get("rewards") or {}).get("reward")
    if reward != 1.0:
        print(t["task_name"])
PY
)

if [ "${#FAILED[@]}" -eq 0 ]; then
  echo "nothing to re-run: every task in $PREV scored 1.0"
  exit 0
fi

echo "re-running ${#FAILED[@]} unsolved task(s) as $NAME"
printf '  %s\n' "${FAILED[@]}"

if [ -n "${DRY_RUN:-}" ]; then
  echo "(DRY_RUN set — not launching)"
  exit 0
fi

INCLUDE=()
for task in "${FAILED[@]}"; do INCLUDE+=(-i "$task"); done

set -a
# shellcheck disable=SC1090
. ~/.bough/env
set +a
export PYTHONPATH="$ROOT"

exec harbor run \
  -d terminal-bench@2.0 \
  --agent bench.harbor.bough_agent:Bough \
  --model "${MODEL:-deepseek/deepseek-v4-flash}" \
  --ak binary="$ROOT/bench/harbor/dist/bough-linux-x86_64" \
  --ak attempts=3 --ak timeout=900 --ak budget=2700 \
  --agent-timeout-multiplier 3.5 \
  "${INCLUDE[@]}" \
  -n "${CONCURRENCY:-4}" -k 1 -q \
  --jobs-dir "$JOBS" --job-name "$NAME" -y
