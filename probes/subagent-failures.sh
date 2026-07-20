#!/usr/bin/env bash
# Tier-3 shell-use RENDERING tests for subagent failure & interruption
# (E3–E6 in docs/subagent-failure-testing.md). Unlike the other probes, this one
# boots its OWN isolated server on a SEEDED DB — the failure states (a subagent
# that errored, was interrupted, posted a long-error note, or is mid-run) can't be
# produced on the live daily-driver server on demand, so we synthesize them with
# no LLM and drive the real TUI against them.
#
#   E3  an interrupted subagent card reads "◼ interrupted" (not "✓ done")
#   E4  a running subagent (⋯ working) transitions to the terminal status
#   E5  a long-error report card is capped + expands on "click to show all"
#   E6  clicking a failed subagent opens it; esc returns to the spawner
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"
PORT="${SAF_PORT:-4397}"
SUHOME="$(mktemp -d "${TMPDIR:-/tmp}/su-saf.XXXXXX")"
STATE="$(mktemp -d "${TMPDIR:-/tmp}/saf-state.XXXXXX")"
DB="$STATE/bough.db"
su() { HOME="$SUHOME" shell-use --session saf "$@"; }

cleanup() {
  su close >/dev/null 2>&1 || true
  HOME="$SUHOME" shell-use daemon stop >/dev/null 2>&1 || true
  kill "$(lsof -ti ":$PORT" 2>/dev/null)" 2>/dev/null || true
  rm -rf "$SUHOME" "$STATE"
}
trap cleanup EXIT

boot() { # boot the isolated server, wait until it answers
  BOUGH_CLAWPATROL=0 BOUGH_PORT="$PORT" BOUGH_DB="$DB" \
    nohup deno run --allow-net --allow-env --allow-read --allow-write \
    --allow-ffi --allow-sys --allow-run "$REPO/src/server/main.ts" \
    >"$STATE/srv.log" 2>&1 &
  for _ in $(seq 1 40); do
    curl -sf "http://127.0.0.1:$PORT/skills" >/dev/null 2>&1 && return 0
    sleep 0.5
  done
  echo "FAIL: isolated server never came up (:$PORT)" >&2
  cat "$STATE/srv.log" >&2
  exit 4
}

stop_server() { kill "$(lsof -ti ":$PORT" 2>/dev/null)" 2>/dev/null || true; sleep 1; }

# --- create schema, then seed the failure states (no LLM) --------------------
boot >/dev/null
stop_server

python3 - "$DB" <<'PY'
import sqlite3, json, sys
db = sqlite3.connect(sys.argv[1]); now = 1784560000000
def sess(i, title, kind, oid=None, omid=None):
    db.execute("INSERT INTO sessions (id,parent_id,title,kind,created_at,workspace,origin_id,origin_message_id) "
               "VALUES (?,?,?,?,?,?,?,?)", (i, None, title, kind, now, "/tmp/ws", oid, omid))
def msg(i, sid, role, parts, pending, t):
    db.execute("INSERT INTO messages (id,session_id,role,parts,pending,created_at) VALUES (?,?,?,?,?,?)",
               (i, sid, role, json.dumps(parts), pending, t))
def turn(i, sid, mid, status):
    db.execute("INSERT INTO turns (id,session_id,message_id,status,step,updated_at,first_output_at) "
               "VALUES (?,?,?,?,?,?,?)", (i, sid, mid, status, "end", now, now))

# spawner root
sess("root", "Refactor auth module", "root")
msg("u1", "root", "user", [{"type": "text", "text": "refactor auth, delegating parts"}], 0, now)
msg("a1", "root", "supervisor", [
    {"type": "text", "text": "delegating four independent parts"},
    {"type": "tool_call", "id": "tc1", "name": "run_steps",
     "input": {"code": "await Promise.allSettled([agent('a'),agent('b'),agent('c'),spawn('d')])"}},
    {"type": "tool_result", "callId": "tc1", "output": "started", "isError": False},
], 0, now + 1000)
turn("t0", "root", "a1", "done")

# E3 — interrupted blocking subagent (no note)
sess("sub-int", "subagent · extract token logic", "subagent", "root", "a1")
msg("i1", "sub-int", "user", [{"type": "text", "text": "extract token logic"}], 0, now + 1100)
msg("i2", "sub-int", "supervisor", [{"type": "text", "text": "⏹ Stopped."}], 0, now + 1200)
turn("t1", "sub-int", "i2", "interrupted")

# E6 — failed blocking subagent (no note), clickable
sess("sub-fail", "subagent · audit the config", "subagent", "root", "a1")
msg("f1", "sub-fail", "user", [{"type": "text", "text": "audit the config"}], 0, now + 1100)
msg("f2", "sub-fail", "supervisor",
    [{"type": "text", "text": "⚠︎ Turn failed: config parser threw on line 42"}], 0, now + 1200)
turn("t2", "sub-fail", "f2", "error")

# E5 — failed DETACHED subagent with a LONG error report (posted as a system note
# in the ROOT thread) — the card must cap the report + offer "click to show all".
report = "\n".join(f"step {n}: checked and it is broken because reason number {n}" for n in range(1, 16))
note = (
    '[subagent finished] "run the migration" (sub-note) — FAILED — its turn errored '
    '(see the report for the error).\n'
    'Changed files on its branch: none.\n'
    f'Report:\n{report}\nZZZ-LAST-REPORT-LINE-marker\n'
    'Its changes stay on its own branch — adopt("sub-note") in run_steps merges them '
    'into this workspace; or leave the branch for review.'
)
sess("sub-note", "subagent · run the migration", "subagent", "root", "a1")
msg("n0", "sub-note", "user", [{"type": "text", "text": "run the migration"}], 0, now + 1100)
turn("t3", "sub-note", "n0", "error")
msg("note1", "root", "system", [{"type": "text", "text": note}], 0, now + 3000)

# E4 — a RUNNING subagent (pending msg + running turn → busy → "⋯ working")
sess("sub-run", "subagent · index the corpus", "subagent", "root", "a1")
msg("r1", "sub-run", "user", [{"type": "text", "text": "index the corpus"}], 0, now + 1100)
msg("r2", "sub-run", "supervisor", [{"type": "text", "text": "indexing..."}], 1, now + 1200)
turn("t4", "sub-run", "r2", "running")

db.commit()
print("seeded:", [r[0] for r in db.execute("SELECT id FROM sessions")])
PY

boot >/dev/null
echo "isolated server up on :$PORT"

# --- drive the TUI ------------------------------------------------------------
su run bough --cwd "$STATE" --env HOME="$SUHOME" --env BOUGH_PORT="$PORT" --cols 110 --rows 40 >/dev/null
su wait text "resume" --timeout 15000 || true
su press Ctrl+P; sleep 1        # sessions panel
su press Enter; sleep 2         # open the first (root) session
su wait idle

pass() { echo "  PASS $1"; }
shot() { su screenshot; }

# E3: the interrupted subagent reads "◼ interrupted", NOT "✓ done".
su expect text "◼ interrupted" --no-strict
shot | grep -q "extract token logic" || { echo "FAIL E3: interrupted card missing" >&2; exit 1; }
pass "E3 interrupted card shows ◼ interrupted"

# E6: the failed blocking subagent reads "✗ failed".
su expect text "✗ failed" --no-strict
pass "E6a failed blocking card shows ✗ failed"

# E5: the long-error detached card is capped and offers to expand.
su expect text "click to show all" --no-strict
su expect text "ZZZ-LAST-REPORT-LINE-marker" --not --no-strict   # capped: last line hidden
su mouse click --on-text "click to show all"; sleep 1
su expect text "ZZZ-LAST-REPORT-LINE-marker" --no-strict          # expanded: now visible
pass "E5 long-error report capped, then expands on click"

# E6: click the failed blocking card → opens the subagent (its Turn-failed text),
# then esc returns to the spawner.
su mouse click --on-text "audit the config"; sleep 2
su expect text "Turn failed" --no-strict
su expect text "config parser threw" --no-strict
su press Escape; sleep 2
su expect text "Refactor auth" --no-strict   # back on the spawner (title truncates)
su expect text "extract token logic" --no-strict   # the spawner's branch cards are back
pass "E6 click-into-failed opens it; esc returns to spawner"

# E4: a subagent that was mid-run when the server died transitions to a terminal
# state. Seeded "running", it is orphaned by recoverOrphanedTurns on boot — the
# card reads ORPHANED (not a stuck ⋯ working), and a note lands in the spawner's
# thread (the D1 restart-recovery UX, rendered end-to-end).
su expect text "ORPHANED" --no-strict
shot | grep -q "index the corpus" || { echo "FAIL E4: orphaned card missing" >&2; exit 1; }
pass "E4 a run stranded by a restart renders ORPHANED (+ note to the spawner)"

echo "PASS — all subagent-failure rendering probes (E3–E6)"
