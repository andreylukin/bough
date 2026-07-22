#!/usr/bin/env bash
# One bough trial, driven through the headless CLI: run-bough.sh <task> [trial-index]
# Needs the bench server up (server.sh start). Emits one JSON result line on stdout.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TASK="$1"
TRIAL="${2:-1}"
curl -sf "$API/sessions" >/dev/null || { echo "bench server not up — bench/server.sh start" >&2; exit 4; }

WORK="$(stage_fixture "$TASK")"
trap 'rm -rf "$WORK"' EXIT

# The whole session-create → net-yolo → stream → wait dance lives in the CLI now
# (src/cli/exec.ts); the bench keeps only staging, grading, and pricing.
T0="$(now_ms)"
# --prompt-dir pins the variant's prompt on THIS session (no server restart), so a
# single long-lived bench server can serve every variant in a campaign. Falls back
# to the server's default prompt when BOUGH_PROMPT_DIR is unset.
ENVELOPE="$(BOUGH_PORT="$PORT" deno run --no-prompt --allow-net=127.0.0.1 --allow-env --allow-read \
  "$BENCH/../src/cli/exec.ts" --json --yolo --timeout "$TRIAL_TIMEOUT" -w "$WORK" \
  ${BOUGH_PROMPT_DIR:+--prompt-dir "$BOUGH_PROMPT_DIR"} \
  "$(cat "$BENCH/tasks/$TASK/prompt.md")")" || true
T1="$(now_ms)"

SID="$(printf '%s' "$ENVELOPE" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("session",""))' 2>/dev/null || true)"
[ -n "$SID" ] || { echo "prompt CLI returned no session: $ENVELOPE" >&2; exit 5; }

# After the first turn the session's workspace column points at its shadow worktree.
WS="$(curl -sf "$API/sessions/$SID" | python3 -c 'import json,sys; print(json.load(sys.stdin)["session"].get("workspace",""))')"
[ -n "$WS" ] || WS="$WORK"
PASS="$(verify_task "$TASK" "$WS")"
REASON=""
STATUS="$(printf '%s' "$ENVELOPE" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("status","done"))')"
[ "$PASS" = 0 ] && REASON="$(fail_reason "$TASK" "$WS" "$STATUS")"

curl -sf -X POST "$API/sessions/$SID/archive" >/dev/null 2>&1 || true

ENVELOPE="$ENVELOPE" TASK="$TASK" TRIAL="$TRIAL" PASS="$PASS" REASON="$REASON" \
WALL=$((T1 - T0)) MODEL="$MODEL_BOUGH" VARIANT="${BENCH_VARIANT:-}" \
python3 - <<'PY'
import json, os, time

e = json.loads(os.environ["ENVELOPE"])
tin = e.get("input_tokens") or 0
tout = e.get("output_tokens") or 0
cread = e.get("cache_read_tokens") or 0
cwrite = e.get("cache_write_tokens") or 0
# $/Mtok (in, out) by model prefix; cache reads bill 0.1x in, writes 1.25x in.
# Sonnet 5 uses the intro price (through 2026-08-31) to match CC's provider-
# reported actuals. bough's input_tokens is total prompt (uncached+reads+writes).
PRICES = [("claude-haiku-4-5", 1, 5), ("claude-sonnet-5", 2, 10), ("claude-opus-4-8", 5, 25),
          ("openai/gpt-oss-120b", 0.037, 0.17)]
pin, pout = next(((i, o) for k, i, o in PRICES if os.environ["MODEL"].startswith(k)), (1, 5))
uncached = max(0, tin - cread - cwrite)
cost = (uncached + cread * 0.1 + cwrite * 1.25) * pin * 1e-6 + tout * pout * 1e-6
print(json.dumps({
    "ts": int(time.time()),
    "harness": "bough",
    "task": os.environ["TASK"],
    "trial": int(os.environ["TRIAL"]),
    "model": os.environ["MODEL"],
    "pass": int(os.environ["PASS"]),
    "fail_reason": os.environ["REASON"] or None,
    "status": e.get("status", "done"),
    "wall_ms": int(os.environ["WALL"]),
    "tokens_in": tin,
    "tokens_in_cached": (cread + cwrite) or None,
    "tokens_out": tout,
    "cost_usd": round(cost, 6),
    "turns": e.get("turns"),
    "tool_calls": e.get("tool_calls"),
    "session": e.get("session"),
    "variant": os.environ.get("VARIANT") or None,
}))
PY
