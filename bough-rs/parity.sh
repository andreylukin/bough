#!/usr/bin/env bash
# Wire-parity harness: boot the TS server and the Rust server side by side on
# scratch data roots, hit the same routes on both, and diff the JSON key shapes
# (and status codes). The server/TUI split is the rewrite's parity anchor
# (ARCHITECTURE.md §0) — this is the check that keeps it honest.
#
#   ./parity.sh            # exit 0 when every probed route matches
set -uo pipefail

RS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$RS_DIR")"
BIN="$RS_DIR/target/release/bough"
TS_PORT="${TS_PORT:-43301}"
RS_PORT="${RS_PORT:-43302}"
[ -x "$BIN" ] || { echo "parity: $BIN missing — run make rs-release first" >&2; exit 2; }

TS_HOME="$(mktemp -d)"; RS_HOME="$(mktemp -d)"
BOUGH_HOME="$TS_HOME" BOUGH_PORT="$TS_PORT" bun "$REPO/src/server/main.ts" >/tmp/parity-ts.log 2>&1 &
TS_PID=$!
BOUGH_HOME="$RS_HOME" BOUGH_PORT="$RS_PORT" "$BIN" start >/tmp/parity-rs.log 2>&1 &
RS_PID=$!
cleanup() { kill "$TS_PID" "$RS_PID" 2>/dev/null || true; }
trap cleanup EXIT

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$TS_PORT/sessions" >/dev/null 2>&1 \
    && curl -sf "http://127.0.0.1:$RS_PORT/sessions" >/dev/null 2>&1 && break
  sleep 0.3
done
curl -sf "http://127.0.0.1:$TS_PORT/sessions" >/dev/null || { echo "parity: TS server never came up (see /tmp/parity-ts.log)" >&2; exit 1; }
curl -sf "http://127.0.0.1:$RS_PORT/sessions" >/dev/null || { echo "parity: Rust server never came up (see /tmp/parity-rs.log)" >&2; exit 1; }

# Give each server one session with one message, so session-scoped routes have
# something real to answer with.
mkses() {
  curl -s -X POST "http://127.0.0.1:$1/sessions" -H 'content-type: application/json' \
    -d '{"workspace":"/tmp"}' | python3 -c 'import sys,json;print(json.load(sys.stdin)["id"])'
}
TS_SID="$(mkses "$TS_PORT")"; RS_SID="$(mkses "$RS_PORT")"
for p in "$TS_PORT:$TS_SID" "$RS_PORT:$RS_SID"; do
  curl -s -o /dev/null -X POST "http://127.0.0.1:${p%%:*}/sessions/${p##*:}/messages" \
    -H 'content-type: application/json' -d '{"text":"parity probe","draft":true}'
done

# Routes probed on both. %s is replaced by that server's session id.
ROUTES=(
  "GET /sessions"
  "GET /sessions/%s"
  "GET /sessions/%s/messages"
  "GET /sessions/%s/usage"
  "GET /sessions/%s/state"
  "GET /questions"
  "GET /sessions/%s/jobs"
  "GET /sessions/%s/artifacts"
  "GET /sessions/%s/comments"
  "GET /sessions/%s/files"
  "GET /models"
  "GET /model-settings"
  "GET /theme"
  "GET /skills"
  "GET /schedules"
  "GET /workflows"
  "GET /saved-workflows"
  "GET /workflow-settings"
  "GET /mcp/servers"
  "GET /sessions/%s/changes"
  "GET /search?q=parity"
  "GET /files"
  "GET /fs/entries?dir=/tmp"
  "GET /fs/branch?dir=/tmp"
)

python3 - "$TS_PORT" "$RS_PORT" "$TS_SID" "$RS_SID" "${ROUTES[@]}" <<'PY'
import json, subprocess, sys

ts_port, rs_port, ts_sid, rs_sid = sys.argv[1:5]
routes = sys.argv[5:]

def fetch(port, path):
    out = subprocess.run(
        ["curl", "-s", "-w", "\n%{http_code}", f"http://127.0.0.1:{port}{path}"],
        capture_output=True, text=True).stdout
    body, _, code = out.rpartition("\n")
    try:
        return int(code), json.loads(body)
    except Exception:
        return int(code or 0), body

def shape(v, p=""):
    """Key paths + leaf types — the wire contract, independent of values."""
    if isinstance(v, dict):
        out = set()
        for k, sub in v.items():
            out |= shape(sub, f"{p}.{k}" if p else k)
        return out
    if isinstance(v, list):
        # Lists are unordered/variable-length here; fold every element's shape.
        out = {f"{p}[]"}
        for sub in v[:5]:
            out |= shape(sub, f"{p}[]")
        return out
    # JSON has one numeric type: `0` and `0.0` are the same value to every
    # client, and serde emits f64 zeros with a decimal point. Collapsing them
    # keeps the diff about the CONTRACT rather than about float formatting.
    t = type(v).__name__
    if t in ("int", "float"):
        t = "number"
    return {f"{p}:{t}"}

bad = 0
for spec in routes:
    method, path = spec.split(" ", 1)
    ts_code, ts_body = fetch(ts_port, path.replace("%s", ts_sid))
    rs_code, rs_body = fetch(rs_port, path.replace("%s", rs_sid))
    problems = []
    if ts_code != rs_code:
        problems.append(f"status {ts_code} (ts) vs {rs_code} (rs)")
    else:
        ts_shape, rs_shape = shape(ts_body), shape(rs_body)
        missing = sorted(ts_shape - rs_shape)
        extra = sorted(rs_shape - ts_shape)
        if missing:
            problems.append("missing in rust: " + ", ".join(missing[:8]))
        if extra:
            problems.append("only in rust: " + ", ".join(extra[:8]))
    if problems:
        bad += 1
        print(f"FAIL  {method} {path}")
        for pr in problems:
            print(f"        {pr}")
    else:
        print(f"ok    {method} {path}  [{ts_code}]")

print()
print(f"parity: {len(routes) - bad}/{len(routes)} routes match")
sys.exit(1 if bad else 0)
PY
