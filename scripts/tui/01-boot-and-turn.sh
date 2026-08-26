#!/usr/bin/env bash
# V1 — boot, composer, a streaming turn, and the ledger steps it left behind.
#
# The replay half asserts that the answer is visible BEFORE it is complete: `llm-replay`'s
# per-chunk `delay_ms` exists for exactly this assertion. The live half (BOUGH_LIVE=1) asks haiku
# for a fixed token and asserts that token plus a real `thought/text` step.
source "$(dirname "$0")/lib.sh"

tui_open
tui_start

t the_tui_boots_into_a_strip_and_a_focus_pane \
  see "sol" --timeout 20000

if [ -z "$BOUGH_LIVE" ]; then
  shell-use type "say the whole sentence please"
  t the_composer_accepts_a_message_and_enter_sends_it \
    see "say the whole sentence please" --timeout 5000
  shell-use press Enter

  # The first fragment is on screen while the rest of the sentence is not yet: that is streaming.
  # Seeing the fragment alone would be vacuous — it is a substring of the finished sentence — so
  # the tail must be ABSENT at the same moment. The fixture's 1500ms `delay_ms` is the window.
  streams_before_complete() {
    see "the first fragment" --timeout 10000 || return 1
    expect_absent "and the rest of it" --timeout 1
  }
  t the_answer_streams_in_before_it_is_complete streams_before_complete
  t the_whole_answer_is_on_screen_when_the_wake_ends \
    see "the first fragment and the rest of it" --timeout 30000
else
  # The token is asked for UPPERCASED and written lowercase in the prompt on purpose: the sent
  # message is itself echoed into the trajectory, so asserting a token that appears verbatim in
  # the prompt would pass on the echo alone and prove nothing about the model. Only haiku can put
  # `BOUGHLIVEOK` on this screen.
  shell-use type "Reply with exactly this token, uppercased, and nothing else: boughliveok"
  t the_composer_accepts_a_message_and_enter_sends_it \
    see "boughliveok" --timeout 5000
  shell-use press Enter
  skip the_answer_streams_in_before_it_is_complete "the live half cannot pin a chunk boundary"
  t a_live_haiku_answer_streams_into_the_focus_pane \
    see "BOUGHLIVEOK" --timeout 60000
  t the_whole_answer_is_on_screen_when_the_wake_ends \
    shell-use wait idle --timeout 60000
fi

t the_status_glyph_returned_to_idle \
  see "idle" --timeout 30000

tui_quit

# What the screen showed is in the ledger: one wake, spliced mail, model text, one wake end.
t the_turn_landed_as_ledger_steps expect_steps "wake/start" 1
t the_turn_landed_as_ledger_steps.inbox expect_steps "inbox/spliced" 1
t the_turn_landed_as_ledger_steps.thought expect_steps "thought/text" 1
t the_turn_landed_as_ledger_steps.end expect_steps "wake/end" 1
