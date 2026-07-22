#!/usr/bin/env bash
# One Claude Code trial: run-cc.sh <task> [trial-index]
# Emits one JSON result line on stdout; all noise goes to stderr.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TASK="$1"
TRIAL="${2:-1}"
WORK="$(stage_fixture "$TASK")"
trap 'rm -rf "$WORK"' EXIT

T0="$(now_ms)"
set +e
# --setting-sources project: exclude the operator's global ~/.claude config so
# both harnesses see only what's in the fixture (auth still applies).
# stream-json (NDJSON) captures the full trajectory for behave.py; the final
# "result" line carries the same envelope the old json format did.
mkdir -p "$RESULTS/cc-traj"
TRAJ="$RESULTS/cc-traj/$TASK-$TRIAL-$(now_ms).jsonl"
(cd "$WORK" && timeout "$TRIAL_TIMEOUT" claude -p "$(cat "$BENCH/tasks/$TASK/prompt.md")" \
  --model "$MODEL_CC" --output-format stream-json --verbose \
  --dangerously-skip-permissions --setting-sources project \
  --max-budget-usd 2 2>/dev/null) >"$TRAJ"
RC=$?
OUT="$(grep '"type":"result"' "$TRAJ" | tail -1 || true)"
set -e
T1="$(now_ms)"
PASS="$(verify_task "$TASK" "$WORK")"
REASON=""
if [ "$PASS" = 0 ]; then
  [ "$RC" = 124 ] && ST=timeout || ST=done
  REASON="$(fail_reason "$TASK" "$WORK" "$ST")"
fi

OUT="$OUT" TASK="$TASK" TRIAL="$TRIAL" PASS="$PASS" RC="$RC" REASON="$REASON" WALL=$((T1 - T0)) MODEL="$MODEL_CC" TRAJ="$TRAJ" \
python3 - <<'PY'
import json, os, time

raw, r = os.environ["OUT"], {}
try:
    r = json.loads(raw)
except Exception:
    pass
usage = r.get("usage", {})
print(json.dumps({
    "ts": int(time.time()),
    "harness": "claude-code",
    "task": os.environ["TASK"],
    "trial": int(os.environ["TRIAL"]),
    "model": os.environ["MODEL"],
    "pass": int(os.environ["PASS"]),
    "fail_reason": os.environ["REASON"] or None,
    "status": r.get("subtype") or f"exit-{os.environ['RC']}",
    "wall_ms": int(os.environ["WALL"]),
    "tokens_in": usage.get("input_tokens"),
    "tokens_in_cached": (usage.get("cache_read_input_tokens") or 0) + (usage.get("cache_creation_input_tokens") or 0),
    "tokens_out": usage.get("output_tokens"),
    "cost_usd": r.get("total_cost_usd"),
    "turns": r.get("num_turns"),
    "transcript": os.environ.get("TRAJ") or None,
}))
PY
