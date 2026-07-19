#!/usr/bin/env bash
# One bough trial, driven headlessly over the server API: run-bough.sh <task> [trial-index]
# Needs the bench server up (server.sh start). Emits one JSON result line on stdout.
source "$(dirname "${BASH_SOURCE[0]}")/lib.sh"

TASK="$1"
TRIAL="${2:-1}"
curl -sf "$API/sessions" >/dev/null || { echo "bench server not up — bench/server.sh start" >&2; exit 4; }

WORK="$(stage_fixture "$TASK")"
SSE="$(mktemp "${TMPDIR:-/tmp}/bench-sse.XXXXXX")"
SSE_PID=""
cleanup() {
  [ -n "$SSE_PID" ] && kill "$SSE_PID" 2>/dev/null || true
  rm -rf "$WORK" "$SSE"
}
trap cleanup EXIT

SID="$(WORK="$WORK" TASK="$TASK" TRIAL="$TRIAL" python3 -c '
import json, os, sys, urllib.request
body = {"title": "bench-" + os.environ["TASK"] + "-" + os.environ["TRIAL"], "workspace": os.environ["WORK"]}
req = urllib.request.Request("'"$API"'/sessions", json.dumps(body).encode(), {"content-type": "application/json"})
print(json.load(urllib.request.urlopen(req))["id"])')"

# Net gate to yolo for this session only — a headless trial can never answer a hold.
curl -sf -X POST "$API/net/yolo" -H 'content-type: application/json' \
  -d "{\"sessionId\":\"$SID\",\"on\":true}" >/dev/null

# Attach the event tail BEFORE the message goes in, so a fast turn can't finish unseen.
curl -sN "$API/events?sessionId=$SID" >"$SSE" 2>/dev/null &
SSE_PID=$!

T0="$(now_ms)"
PROMPT_JSON="$(python3 -c 'import json,sys; print(json.dumps({"text": open(sys.argv[1]).read()}))' "$BENCH/tasks/$TASK/prompt.md")"
curl -sf -X POST "$API/sessions/$SID/messages" -H 'content-type: application/json' -d "$PROMPT_JSON" >/dev/null

STATUS="timeout"
for _ in $(seq 1 "$TRIAL_TIMEOUT"); do
  if grep -q "turn.finished" "$SSE" 2>/dev/null; then
    STATUS="$(python3 - "$SSE" <<'PY'
import json, sys
status = "done"
lines = open(sys.argv[1], errors="replace").read().splitlines()
for i, line in enumerate(lines):
    if "turn.finished" in line and i + 1 < len(lines) and lines[i + 1].startswith("data:"):
        try:
            status = json.loads(lines[i + 1][5:]).get("status", "done")
        except Exception:
            pass
print(status)
PY
)"
    break
  fi
  sleep 1
done
T1="$(now_ms)"
kill "$SSE_PID" 2>/dev/null || true
wait "$SSE_PID" 2>/dev/null || true
SSE_PID=""

# After the first turn the session's workspace column points at its shadow worktree.
WS="$(curl -sf "$API/sessions/$SID" | python3 -c 'import json,sys; print(json.load(sys.stdin)["session"].get("workspace",""))')"
[ -n "$WS" ] || WS="$WORK"
PASS="$(verify_task "$TASK" "$WS")"
REASON=""
[ "$PASS" = 0 ] && REASON="$(fail_reason "$TASK" "$WS" "$STATUS")"
METRICS="$(curl -sf "$API/sessions/$SID/metrics" || echo '{}')"
TOKENS="$(sqlite3 "$STATE/bough.db" "select coalesce(input_tokens,0), coalesce(output_tokens,0) from sessions where id='$SID'" 2>/dev/null || echo '0|0')"

curl -sf -X POST "$API/sessions/$SID/archive" >/dev/null 2>&1 || true

METRICS="$METRICS" TOKENS="$TOKENS" TASK="$TASK" TRIAL="$TRIAL" PASS="$PASS" STATUS="$STATUS" \
REASON="$REASON" WALL=$((T1 - T0)) MODEL="$MODEL_BOUGH" SID="$SID" \
python3 - <<'PY'
import json, os, time

m = json.loads(os.environ["METRICS"] or "{}")
tin, tout = (int(x) for x in os.environ["TOKENS"].split("|"))
# Anthropic list price for Haiku 4.5 ($/Mtok): in 1, out 5 — cache discounts not modeled.
cost = tin * 1e-6 * 1 + tout * 1e-6 * 5
print(json.dumps({
    "ts": int(time.time()),
    "harness": "bough",
    "task": os.environ["TASK"],
    "trial": int(os.environ["TRIAL"]),
    "model": os.environ["MODEL"],
    "pass": int(os.environ["PASS"]),
    "fail_reason": os.environ["REASON"] or None,
    "status": os.environ["STATUS"],
    "wall_ms": int(os.environ["WALL"]),
    "tokens_in": tin,
    "tokens_in_cached": None,
    "tokens_out": tout,
    "cost_usd": round(cost, 6),
    "turns": m.get("assistantTurns"),
    "tool_calls": m.get("toolCalls"),
    "session": os.environ["SID"],
}))
PY
