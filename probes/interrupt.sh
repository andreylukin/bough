#!/usr/bin/env bash
# Interrupt probe: Esc mid-stream must stop the turn fast and leave the session
# coherent. This is the "time-to-interrupt" metric — the cost of pulling the
# brake, which observable-autonomy designs keep far below the cost of undoing.
#
# Asserts: busy indicator appears → Esc → "⏹ Stopped." within 10s → composer is
# usable again → the server recorded the turn as interrupted (not error).
cd "$(dirname "$0")"
source ./lib.sh

trap 'probe_cleanup "${SID:-}"' EXIT
probe_start

# Waiting on chrome ("esc interrupts") is flaky — the status row truncates when
# the activity blurb is long. Instead have the reply START with a unique token
# (reversed word, so it isn't in the echoed prompt) and interrupt once streaming
# is visibly underway.
su submit "Spell the word stream backwards, then immediately continue with a 400-word story about a mountain, plain prose. Do not use any tools."
su wait text "maerts" --timeout 60000
T0="$(now_ms)"
su press Escape
su wait text "⏹ Stopped." --timeout 10000
T1="$(now_ms)"
echo "esc → stopped visible: $((T1 - T0)) ms"

# The composer must be live again (the turn released the input).
su expect text "›" --no-strict

SID="$(probe_session_id)"
[ -n "$SID" ] || { echo "FAIL: probe session not found on server" >&2; exit 1; }
probe_metrics "$SID" | python3 -c "
import json, sys
m = json.load(sys.stdin)
assert m['interrupted'] >= 1, f'server did not record the interrupt: {m}'
assert m['failed'] == 0, f'interrupt was recorded as a FAILURE: {m}'
print(f\"server: interrupted={m['interrupted']} failed={m['failed']} — ok\")
"
echo PASS
