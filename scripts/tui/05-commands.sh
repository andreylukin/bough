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

shell-use submit "/agents"
t a_slash_command_renders_its_output_in_the_pane \
  see "sol" --timeout 10000

t the_slash_command_started_no_wake \
  bash -c "[ \"\$(steps_of 'wake/start')\" = \"$before_wakes\" ] \
        && [ \"\$(steps_of 'step/start')\" = \"$before_steps\" ] \
        && [ \"\$(steps_of 'request/header')\" = \"$before_reqs\" ]"

shell-use submit "/nosuchcommand"
t an_unknown_command_reports_an_error_inline \
  see "unknown" --timeout 10000

shell-use submit "/help"
t help_lists_the_registered_commands \
  see "/quit" --timeout 10000

tui_quit
