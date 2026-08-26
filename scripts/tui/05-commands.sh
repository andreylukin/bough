#!/usr/bin/env bash
# V5 — `ctx.commands` dispatches a human command WITHOUT a model turn (P3-D8). The proof is the
# ledger read before and after: no new `wake/start`, no new `step/start`, no new `request/header`.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip a_slash_command_renders_its_output_in_the_pane "commands never call a model"; exit 0; }

tui_open
tui_start

before_wakes="$(steps_of 'wake/start')"
before_steps="$(steps_of 'step/start')"
before_reqs="$(steps_of 'request/header')"

# `queued` is written ONLY by the `/agents` roster command (builtins.rs::Roster). The agent name
# `sol` would have matched the rail, which is on screen before any command runs, so that bullet
# was vacuous whether or not the dispatch produced anything.
shell-use submit "/agents"
t a_slash_command_renders_its_output_in_the_pane \
  see "queued" --timeout 10000

t the_slash_command_started_no_wake \
  bash -c "[ \"\$(steps_of 'wake/start')\" = \"$before_wakes\" ] \
        && [ \"\$(steps_of 'step/start')\" = \"$before_steps\" ] \
        && [ \"\$(steps_of 'request/header')\" = \"$before_reqs\" ]"

shell-use submit "/nosuchcommand"
t an_unknown_command_reports_an_error_inline \
  see 'unknown command `nosuchcommand`' --timeout 10000

shell-use submit "/help"
t help_lists_the_registered_commands \
  see "/quit" --timeout 10000

# The CONTROL for the bullet above. Equal counters prove nothing unless the counters can move:
# a PLAIN line (no prefix) is a model turn, and it must raise exactly the three kinds the slash
# commands left alone. Without this, `the_slash_command_started_no_wake` would still pass against
# a ledger that never records anything at all.
shell-use submit "hello"
t a_plain_line_does_start_a_turn_so_the_counters_can_move \
  bash -c "for i in \$(seq 1 60); do \
             [ \"\$(steps_of 'wake/start')\" -gt $before_wakes ] \
          && [ \"\$(steps_of 'step/start')\" -gt $before_steps ] \
          && [ \"\$(steps_of 'request/header')\" -gt $before_reqs ] && exit 0; sleep 0.5; done; \
           echo \"wake/start=\$(steps_of 'wake/start') step/start=\$(steps_of 'step/start') request/header=\$(steps_of 'request/header')\"; exit 1"

tui_quit
