#!/usr/bin/env bash
# Headless exec probe: `bough exec` round-trip against the live server.
#
# Exercises both prompt forms — positional argument with --json (asserts a
# clean envelope: status done, session id, token usage) and a stdin pipe with
# streaming (asserts the reply text lands on stdout). No TUI involved; this
# guards the scripting/bench transport.
cd "$(dirname "$0")"
source ./lib.sh

ROOT="$(cd .. && pwd)"
WS="$(mktemp -d "${TMPDIR:-/tmp}/bough-probe.XXXXXX")"
SID1=""
SID2=""
cleanup() {
  for sid in "$SID1" "$SID2"; do
    [ -n "$sid" ] && curl -sf -X POST "$API/sessions/$sid/archive" >/dev/null 2>&1 || true
  done
  rm -rf "$WS"
}
trap cleanup EXIT

curl -sf "$API/sessions" >/dev/null || {
  echo "bough server not reachable on :$PORT — bough start" >&2
  exit 4
}

run_exec() {
  (cd "$ROOT" && deno run --no-prompt --allow-net=127.0.0.1 --allow-env --allow-read \
    src/cli/exec.ts "$@")
}

PROMPT="Spell the word zebra backwards. Reply with just that lowercase word, no tools."

# Argument form, --json envelope.
T0="$(now_ms)"
ENVELOPE="$(run_exec --json -w "$WS" "$PROMPT")"
T1="$(now_ms)"
SID1="$(printf '%s' "$ENVELOPE" | python3 -c 'import json,sys; print(json.load(sys.stdin).get("session",""))')"
printf '%s' "$ENVELOPE" | python3 -c "
import json, sys
e = json.load(sys.stdin)
assert e.get('status') == 'done', f\"status={e.get('status')}\"
assert e.get('session'), 'no session id'
assert (e.get('output_tokens') or 0) > 0, 'no output tokens'
" || { echo "FAIL: bad --json envelope: $ENVELOPE" >&2; exit 1; }
echo "json form: done in $((T1 - T0)) ms ($ENVELOPE)"

# Stdin form, streaming.
OUT="$(printf '%s' "$PROMPT" | run_exec -w "$WS")"
SID2="$(probe_session_id)"
case "$OUT" in
  *arbez*) echo "stdin form: reply streamed to stdout" ;;
  *) echo "FAIL: streamed output missing reply: $OUT" >&2; exit 1 ;;
esac
