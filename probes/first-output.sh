#!/usr/bin/env bash
# Time-to-first-output probe: how long the user stares at a blank turn.
#
# Measures two numbers for one trivial prompt:
#   - end-to-end: submit keystroke → reply text visible in the TUI (shell-use
#     wall clock; includes wait-poll granularity, so treat as an upper bound)
#   - server-side: user message → turns.first_output_at (the metrics endpoint),
#     which excludes TUI render time and isolates the harness+model share.
# A growing gap between the two = TUI rendering latency.
cd "$(dirname "$0")"
source ./lib.sh

trap 'probe_cleanup "${SID:-}"' EXIT
probe_start

# The expected reply must never appear in the prompt itself (the prompt is
# echoed into the thread) NOR collide with chrome like the token counter (a
# bare number can — the status bar once read "● 777"). A reversed word is
# absent from both.
T0="$(now_ms)"
su submit "Spell the word zebra backwards. Reply with just that lowercase word, no tools."
su wait text "arbez" --timeout 90000
T1="$(now_ms)"

SID="$(probe_session_id)"
[ -n "$SID" ] || { echo "FAIL: probe session not found on server" >&2; exit 1; }
# Let the turn finish so the metrics row is final.
su wait text "esc interrupts" --not --timeout 90000 || true

echo "end-to-end (submit → visible): $((T1 - T0)) ms"
probe_metrics "$SID" | python3 -c "
import json, sys
m = json.load(sys.stdin)
fo = m.get('firstOutput')
print(f\"server-side (message → first output): {fo['medianMs'] if fo else '(not stamped)'} ms\")
print(f\"turns={m['userTurns']} toolCalls={m['toolCalls']} approvalPrompts={m['approvalPrompts']}\")
"
