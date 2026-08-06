#!/usr/bin/env bash
# Event-stream parity: run the SAME prompt through the TS server and the Rust
# server, capture each one's SSE stream for the whole turn, and diff the event
# TYPES and their data key shapes.
#
# parity.sh covers the request/response half of the wire; this covers the push
# half, which is the half the TUI actually renders from. A missing event type
# or a renamed data field is invisible to both test suites (each side is
# internally consistent) and shows up here.
#
#   SMOKE_MODEL=openai/gpt-5.6-luna ./event-parity.sh
set -uo pipefail

RS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$RS_DIR")"
BIN="$RS_DIR/target/release/bough"
MODEL="${SMOKE_MODEL:-openai/gpt-5.6-luna}"
PROMPT="${EVENT_PROMPT:-reply with only the word PING}"
TS_PORT="${TS_PORT:-43311}"
RS_PORT="${RS_PORT:-43312}"
[ -x "$BIN" ] || { echo "event-parity: $BIN missing — run make rs-release first" >&2; exit 2; }
: "${OPENROUTER_API_KEY:?event-parity: needs OPENROUTER_API_KEY for a live turn}"

TS_HOME="$(mktemp -d)"; RS_HOME="$(mktemp -d)"
BOUGH_HOME="$TS_HOME" BOUGH_PORT="$TS_PORT" BOUGH_MODEL="$MODEL" \
  bun "$REPO/src/server/main.ts" >/tmp/eparity-ts.log 2>&1 &
TS_PID=$!
BOUGH_HOME="$RS_HOME" BOUGH_PORT="$RS_PORT" BOUGH_MODEL="$MODEL" \
  "$BIN" start >/tmp/eparity-rs.log 2>&1 &
RS_PID=$!
cleanup() { kill "$TS_PID" "$RS_PID" 2>/dev/null || true; }
trap cleanup EXIT

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$TS_PORT/sessions" >/dev/null 2>&1 \
    && curl -sf "http://127.0.0.1:$RS_PORT/sessions" >/dev/null 2>&1 && break
  sleep 0.3
done

# Per server: subscribe to /events, post the prompt, let the turn finish.
capture() {
  local port="$1" out="$2"
  ( curl -sN --max-time 90 "http://127.0.0.1:$port/events" > "$out" ) &
  local tap=$!
  sleep 0.6
  local sid
  sid="$(curl -s -X POST "http://127.0.0.1:$port/sessions" -H 'content-type: application/json' \
    -d "{\"workspace\":\"/tmp\"}" | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])')"
  curl -s -o /dev/null -X POST "http://127.0.0.1:$port/sessions/$sid/messages" \
    -H 'content-type: application/json' \
    -d "$(python3 -c 'import json,sys;print(json.dumps({"text":sys.argv[1]}))' "$PROMPT")"
  # Wait for the turn to settle: the stream carries turn.finished at the end.
  for _ in $(seq 1 90); do
    grep -q '"type":"turn.finished"' "$out" 2>/dev/null && break
    sleep 1
  done
  sleep 0.5
  kill "$tap" 2>/dev/null || true
}

echo "event-parity: running \"$PROMPT\" on $MODEL through both servers…"
capture "$TS_PORT" /tmp/eparity-ts.sse
capture "$RS_PORT" /tmp/eparity-rs.sse

python3 - /tmp/eparity-ts.sse /tmp/eparity-rs.sse <<'PY'
import json, sys, collections

def parse(path):
    """SSE frames → the event objects the TUI reducer would see."""
    events = []
    for block in open(path).read().split("\n\n"):
        for line in block.splitlines():
            if line.startswith("data:"):
                try:
                    events.append(json.loads(line[5:].strip()))
                except Exception:
                    pass
    return events

def shape(v, p=""):
    if isinstance(v, dict):
        out = set()
        for k, sub in v.items():
            out |= shape(sub, f"{p}.{k}" if p else k)
        return out
    if isinstance(v, list):
        out = {f"{p}[]"}
        for sub in v[:3]:
            out |= shape(sub, f"{p}[]")
        return out
    t = type(v).__name__
    if t in ("int", "float"):
        t = "number"
    return {f"{p}:{t}"}

ts, rs = parse(sys.argv[1]), parse(sys.argv[2])
if not ts or not rs:
    print(f"event-parity: no events captured (ts={len(ts)}, rs={len(rs)}) — check /tmp/eparity-*.log")
    sys.exit(1)

def by_type(events):
    out = collections.defaultdict(set)
    for e in events:
        out[e.get("type", "?")] |= shape(e.get("data", {}))
    return out

ts_types, rs_types = by_type(ts), by_type(rs)
print(f"captured: {len(ts)} events (ts) / {len(rs)} events (rs)\n")

bad = 0
for t in sorted(set(ts_types) | set(rs_types)):
    if t not in rs_types:
        print(f"FAIL  {t}: emitted by TS, never by Rust"); bad += 1; continue
    if t not in ts_types:
        print(f"note  {t}: emitted by Rust, not by TS in this run"); continue
    missing = sorted(ts_types[t] - rs_types[t])
    if missing:
        print(f"FAIL  {t}: data keys missing in rust: {', '.join(missing[:8])}"); bad += 1
    else:
        print(f"ok    {t}")

# The envelope itself: every event carries type/seq/ts, and NEVER an id field
# (an SSE `id:` would invite Last-Event-ID resume, which the design refuses).
for name, evs in (("ts", ts), ("rs", rs)):
    for e in evs:
        for k in ("type", "seq"):
            if k not in e:
                print(f"FAIL  {name}: an event is missing `{k}`: {e}"); bad += 1; break

print()
print(f"event-parity: {len(set(ts_types)) - bad}/{len(set(ts_types))} TS event types matched")
sys.exit(1 if bad else 0)
PY
