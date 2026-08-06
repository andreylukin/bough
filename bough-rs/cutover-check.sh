#!/usr/bin/env bash
# Cutover rehearsal (PORT_PLAN gate G3): prove the Rust server can open the
# user's REAL database and serve what the TS server serves from it.
#
# Runs entirely on COPIES — the live ~/.bough is never opened by either server,
# because a migrate bug on the real file is unrecoverable and this check exists
# precisely because we do not yet trust the migrate.
set -uo pipefail

RS_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO="$(dirname "$RS_DIR")"
BIN="$RS_DIR/target/release/bough"
LIVE="${BOUGH_LIVE_DB:-$HOME/.bough/bough.db}"
[ -x "$BIN" ] || { echo "cutover: $BIN missing — run make rs-release first" >&2; exit 2; }
[ -f "$LIVE" ] || { echo "cutover: no live db at $LIVE — nothing to rehearse"; exit 0; }

RS_HOME="$(mktemp -d)"; TS_HOME="$(mktemp -d)"
cp "$LIVE" "$RS_HOME/bough.db"
cp "$LIVE" "$TS_HOME/bough.db"
BEFORE_SESSIONS="$(sqlite3 "$RS_HOME/bough.db" 'select count(*) from sessions')"
BEFORE_MESSAGES="$(sqlite3 "$RS_HOME/bough.db" 'select count(*) from messages')"
BEFORE_VERSION="$(sqlite3 "$RS_HOME/bough.db" 'pragma user_version')"
echo "cutover: rehearsing on a copy — $BEFORE_SESSIONS sessions, $BEFORE_MESSAGES messages, user_version $BEFORE_VERSION"

BOUGH_HOME="$RS_HOME" BOUGH_PORT=43290 "$BIN" start >/tmp/cutover-rs.log 2>&1 &
RS_PID=$!
BOUGH_HOME="$TS_HOME" BOUGH_PORT=43291 bun "$REPO/src/server/main.ts" >/tmp/cutover-ts.log 2>&1 &
TS_PID=$!
cleanup() { kill "$RS_PID" "$TS_PID" 2>/dev/null || true; }
trap cleanup EXIT

for _ in $(seq 1 60); do
  curl -sf http://127.0.0.1:43290/sessions >/dev/null 2>&1 \
    && curl -sf http://127.0.0.1:43291/sessions >/dev/null 2>&1 && break
  sleep 0.4
done

count() { curl -s "http://127.0.0.1:$1/sessions" | python3 -c 'import sys,json;print(len(json.load(sys.stdin)))'; }
first() { curl -s "http://127.0.0.1:$1/sessions" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(d[0]["id"] if d else "")'; }
RS_N="$(count 43290)"; TS_N="$(count 43291)"
RS_1="$(first 43290)"; TS_1="$(first 43291)"
cleanup; sleep 0.5

fail=0
if [ "$RS_N" != "$TS_N" ]; then
  echo "FAIL  session listing: $TS_N (ts) vs $RS_N (rs)"; fail=1
else
  echo "ok    both servers list $RS_N top-level sessions from the real database"
fi
if [ "$RS_1" != "$TS_1" ]; then
  echo "FAIL  newest session differs: $TS_1 (ts) vs $RS_1 (rs)"; fail=1
else
  echo "ok    same newest session, so the ordering rule survived the port"
fi

# The migrate must not have destroyed or invented rows, and must not have
# stamped a version the TS server would then refuse.
AFTER_SESSIONS="$(sqlite3 "$RS_HOME/bough.db" 'select count(*) from sessions')"
AFTER_MESSAGES="$(sqlite3 "$RS_HOME/bough.db" 'select count(*) from messages')"
AFTER_VERSION="$(sqlite3 "$RS_HOME/bough.db" 'pragma user_version')"
INTEGRITY="$(sqlite3 "$RS_HOME/bough.db" 'pragma integrity_check')"
[ "$INTEGRITY" = "ok" ] || { echo "FAIL  integrity_check: $INTEGRITY"; fail=1; }
[ "$AFTER_SESSIONS" = "$BEFORE_SESSIONS" ] || { echo "FAIL  sessions $BEFORE_SESSIONS -> $AFTER_SESSIONS"; fail=1; }
[ "$AFTER_MESSAGES" = "$BEFORE_MESSAGES" ] || { echo "FAIL  messages $BEFORE_MESSAGES -> $AFTER_MESSAGES"; fail=1; }
[ "$AFTER_VERSION" = "$BEFORE_VERSION" ] || echo "note  user_version $BEFORE_VERSION -> $AFTER_VERSION (expected only on a real migration)"
[ "$fail" = 0 ] && echo "ok    integrity ok, no rows gained or lost"

echo
[ "$fail" = 0 ] && echo "cutover: PASS — the Rust server can serve the live database" \
                || echo "cutover: FAIL"
exit "$fail"
