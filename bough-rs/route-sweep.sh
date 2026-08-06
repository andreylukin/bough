#!/usr/bin/env bash
# The "built but not wired" sweep: probe EVERY route in the table on both
# servers and diff the status codes.
#
# parity.sh probes 24 GET routes and diffs their JSON key shapes. That is the
# read half of the wire, and it cannot see a POST whose handler is a stub —
# `POST /workflows` returning 400 "not yet ported" is invisible to it, and so is
# every verb whose module has green unit tests but whose route was never wired
# to it. This walks the whole table instead: same request to both servers, diff
# the status. A route where the TS server does something and the Rust server
# answers 400/404/501 is a subsystem that exists in bough-core and is
# unreachable from any client.
#
#   ./route-sweep.sh
set -uo pipefail

RS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$RS_DIR")"
BIN="$RS_DIR/target/release/bough"
TS_PORT="${TS_PORT:-43321}"
RS_PORT="${RS_PORT:-43322}"
[ -x "$BIN" ] || { echo "route-sweep: $BIN missing — run make rs-release first" >&2; exit 2; }

TS_HOME="$(mktemp -d)"; RS_HOME="$(mktemp -d)"
BOUGH_HOME="$TS_HOME" BOUGH_PORT="$TS_PORT" bun "$REPO/src/server/main.ts" >/tmp/sweep-ts.log 2>&1 &
TS_PID=$!
BOUGH_HOME="$RS_HOME" BOUGH_PORT="$RS_PORT" "$BIN" start >/tmp/sweep-rs.log 2>&1 &
RS_PID=$!
cleanup() { kill "$TS_PID" "$RS_PID" 2>/dev/null || true; }
trap cleanup EXIT

for _ in $(seq 1 60); do
  curl -sf "http://127.0.0.1:$TS_PORT/sessions" >/dev/null 2>&1 \
    && curl -sf "http://127.0.0.1:$RS_PORT/sessions" >/dev/null 2>&1 && break
  sleep 0.3
done
curl -sf "http://127.0.0.1:$TS_PORT/sessions" >/dev/null || { echo "route-sweep: TS never came up (/tmp/sweep-ts.log)" >&2; exit 1; }
curl -sf "http://127.0.0.1:$RS_PORT/sessions" >/dev/null || { echo "route-sweep: Rust never came up (/tmp/sweep-rs.log)" >&2; exit 1; }

python3 - "$TS_PORT" "$RS_PORT" <<'PY'
import json, subprocess, sys

ts_port, rs_port = sys.argv[1], sys.argv[2]

def req(port, method, path, body=None):
    cmd = ["curl", "-s", "-X", method, "-w", "\n%{http_code}",
           f"http://127.0.0.1:{port}{path}"]
    if body is not None:
        cmd += ["-H", "content-type: application/json", "-d", json.dumps(body)]
    out = subprocess.run(cmd, capture_output=True, text=True).stdout
    text, _, code = out.rpartition("\n")
    return int(code or 0), text

def mkses(port):
    _, t = req(port, "POST", "/sessions", {"workspace": "/tmp"})
    return json.loads(t)["id"]

SID = {ts_port: mkses(ts_port), rs_port: mkses(rs_port)}

# A workflow script the ENGINE accepts: `meta` is read without running the
# script, so a body without it is refused before any wiring question is asked —
# which would make a stubbed route look like a matching 400.
SCRIPT = (
    "export const meta = { name: 'sweep', description: 'the route sweep probe' }\n"
    "return 1\n"
)
for p in (ts_port, rs_port):
    req(p, "POST", f"/sessions/{SID[p]}/messages", {"text": "sweep probe", "draft": True})

# (method, path, body) — %s is that server's session id. Ordered as app.ts
# declares them. `GET /events` is excluded: it is an open stream, and
# event-parity.sh covers it.
PROBES = [
    ("GET",    "/sessions", None),
    ("POST",   "/sessions", {"workspace": "/tmp"}),
    ("GET",    "/sessions/%s", None),
    ("PATCH",  "/sessions/%s", {"title": "swept"}),
    ("POST",   "/sessions/%s/messages", {"text": "probe", "draft": True}),
    ("PUT",    "/sessions/%s/draft", {"draft": "d"}),
    # `draft` is required-and-nullable in the TS schema: a body without the key
    # is a 400 there, and was a silent draft-wipe here until the schema said so.
    ("PUT",    "/sessions/%s/draft", {"text": "typo"}),
    ("GET",    "/workflows", None),
    ("POST",   "/workflows", {"sessionId": "%s", "script": SCRIPT}),
    ("GET",    "/workflows/nope", None),
    ("POST",   "/workflows/nope/stop", None),
    ("POST",   "/workflows/nope/pause", None),
    ("POST",   "/workflows/nope/resume", None),
    ("POST",   "/workflows/nope/rerun", {}),
    ("POST",   "/workflows/nope/agents/a1/stop", None),
    ("GET",    "/schedules", None),
    ("POST",   "/schedules", {"sessionId": "%s", "spec": "every 1h", "prompt": "hi"}),
    ("PATCH",  "/schedules/nope", {"enabled": False}),
    ("DELETE", "/schedules/nope", None),
    ("GET",    "/questions", None),
    ("POST",   "/sessions/%s/questions/q1", {"answer": "yes"}),
    ("GET",    "/sessions/%s/artifacts", None),
    ("GET",    "/artifacts/%s/index.html", None),
    ("GET",    "/sessions/%s/comments", None),
    ("POST",   "/sessions/%s/comments", {"artifact": "a1", "anchor": "h1", "text": "note"}),
    ("POST",   "/sessions/%s/comments/send", {}),
    ("DELETE", "/sessions/%s/comments/nope", None),
    ("GET",    "/sessions/%s/jobs", None),
    ("POST",   "/sessions/%s/jobs", {"cmd": "true", "tags": ["probe"]}),
    ("POST",   "/sessions/%s/jobs/nope/kill", None),
    ("GET",    "/sessions/%s/jobs/nope/output", None),
    ("POST",   "/workflows/nope/relaunch", {}),
    ("GET",    "/workflows/nope/replay", None),
    ("POST",   "/workflows/nope/save", {"name": "n"}),
    ("GET",    "/saved-workflows", None),
    ("GET",    "/saved-workflows/nope", None),
    ("PUT",    "/saved-workflows/swept", {"script": SCRIPT}),
    ("POST",   "/saved-workflows/swept/runs", {"sessionId": "%s"}),
    ("GET",    "/models", None),
    ("GET",    "/model-settings", None),
    ("PUT",    "/model-settings", {"model": "openai/gpt-5.6-luna"}),
    ("GET",    "/sessions/%s/files", None),
    ("GET",    "/files?dir=/tmp", None),
    ("GET",    "/fs/entries?dir=/tmp", None),
    ("GET",    "/fs/branch?dir=/tmp", None),
    ("GET",    "/workflow-settings", None),
    ("PUT",    "/workflow-settings", {"guideline": "default"}),
    ("GET",    "/mcp/servers/nope/auth", None),
    ("POST",   "/mcp/servers/nope/auth", {}),
    ("DELETE", "/mcp/servers/nope/auth", None),
    ("GET",    "/mcp/servers", None),
    ("PUT",    "/mcp/servers", {"name": "swept", "url": "https://example.invalid/mcp"}),
    ("PUT",    "/mcp/servers/swept", {"url": "https://example.invalid/mcp"}),
    ("POST",   "/mcp/servers/nope/connect", {}),
    ("POST",   "/mcp/servers/nope/tools/t", {}),
    ("POST",   "/mcp/servers/nope/restart", {}),
    ("POST",   "/mcp/servers/swept/enable", {}),
    ("POST",   "/mcp/servers/swept/disable", {}),
    ("DELETE", "/mcp/servers/swept", None),
    ("POST",   "/sessions/%s/fork", {"messageId": "nope"}),
    ("POST",   "/sessions/%s/compact", {}),
    ("POST",   "/sessions/%s/sections", {}),
    ("POST",   "/sessions/%s/extract", {"messageIds": []}),
    ("POST",   "/sessions/%s/move-into", {"target": "nope", "messageIds": []}),
    ("POST",   "/sessions/%s/handoff", {}),
    ("GET",    "/sessions/%s/changes", None),
    ("POST",   "/sessions/%s/changes/revert", {"paths": []}),
    ("GET",    "/search?q=probe", None),
    ("POST",   "/search/reindex", {}),
    ("GET",    "/theme", None),
    ("PUT",    "/theme", {"bg": "#101010", "fg": "#e0e0e0"}),
    ("DELETE", "/theme", None),
    ("POST",   "/sessions/%s/ghost", {"text": "gh"}),
    ("GET",    "/skills", None),
    ("GET",    "/skills/nope", None),
    ("POST",   "/sessions/%s/interrupt", None),
    ("GET",    "/sessions/%s/usage", None),
    ("POST",   "/attachments", {"not": "multipart"}),
    ("POST",   "/sessions/%s/unsend", {"messageId": "nope"}),
]

def sub(v, sid):
    if isinstance(v, str):
        return v.replace("%s", sid)
    if isinstance(v, dict):
        return {k: sub(x, sid) for k, x in v.items()}
    return v

bad, stubs = 0, []
for method, path, body in PROBES:
    ts_code, ts_body = req(ts_port, method, sub(path, SID[ts_port]), sub(body, SID[ts_port]))
    rs_code, rs_body = req(rs_port, method, sub(path, SID[rs_port]), sub(body, SID[rs_port]))
    if ts_code == rs_code:
        print(f"ok    {method:6} {path}  [{ts_code}]")
        continue
    bad += 1
    note = ""
    if "not yet ported" in rs_body:
        note = "  ← RUST STUB: 'not yet ported'"
        stubs.append(f"{method} {path}")
    elif ts_code < 400 <= rs_code:
        note = "  ← TS acts, Rust refuses"
        stubs.append(f"{method} {path}")
    print(f"FAIL  {method:6} {path}  ts={ts_code} rs={rs_code}{note}")
    if note:
        print(f"        rs: {rs_body[:200]}")

print()
print(f"route-sweep: {len(PROBES) - bad}/{len(PROBES)} routes answer alike")
if stubs:
    print("\nUNWIRED (a client can reach nothing behind these):")
    for s in stubs:
        print(f"  {s}")
sys.exit(1 if bad else 0)
PY
