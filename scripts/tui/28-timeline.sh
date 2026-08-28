#!/usr/bin/env bash
# Phase c, §11 "Digging" — the cross-agent timeline pane.
#
# The timeline is a pure function of the ledger (its own tests prove that offline). What this
# script owns is the SURFACE: `/timeline` opens it with a filter typed on the command line, the
# filter narrows what is on screen, a bad filter word is refused where it was typed, Esc clears
# the pane's own editor before it gives the keyboard back, and a resize keeps the status line.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && {
  skip_all "the timeline is a lens on a scripted trajectory" \
  the_command_opens_the_timeline_pane \
  a_filter_typed_in_the_pane_narrows_the_rows \
  an_unknown_filter_word_is_refused_with_the_usage \
  esc_clears_the_editor_and_then_gives_the_keyboard_back \
  clicking_a_row_focuses_that_agent_and_step \
  the_pane_collapses_below_its_breakpoint \
  the_status_line_survives_every_size
  exit 0
}

# The pane is a CATALOG row in no bundle (the `tui-probe` precedent): this script mounts it, and
# only it, so the Aux band is not split three ways.
ROW="$(add_row timeline <<'YML'

- id: tui.timeline
  plugin: tui-timeline
  config:
    height: 12
    collapse_rows: 24
    min_rows: 4
    max_rows: 40
    window: 400
    limit: 200
    debounce_ms: 150
    time_format: "%H:%M:%S"
YML
)"

tui_open
tui_start "$ROW"

shell-use submit "say hello"
shell-use wait idle --timeout 30000

rows_count() { shell-use text | grep -oE "[0-9]+ rows" | head -1 | grep -oE "[0-9]+"; }
export -f rows_count

shell-use submit "/timeline"
t the_command_opens_the_timeline_pane \
  bash -c '
    for i in $(seq 1 40); do
      shell-use text | grep -qE "filter \[.*everything .* [0-9]+ rows" && exit 0
      sleep 0.3
    done
    echo "no timeline header on screen"
    exit 1
  '

before_rows="$(rows_count)"
# The command FOCUSED the pane, so the next keys go to the pane's own field — which is the field
# a person would type in. A filter that can match nothing on this tree must move the row count,
# and downwards.
shell-use type "agent:nobody"
shell-use press Enter
t a_filter_typed_in_the_pane_narrows_the_rows \
  bash -c '
    for i in $(seq 1 40); do
      now="$(rows_count)"
      [ -n "$now" ] && [ "$now" -lt "'"$before_rows"'" ] && exit 0
      sleep 0.3
    done
    echo "the row count never fell below '"$before_rows"' (now $(rows_count))"
    exit 1
  '

# Back to the composer before the next command: the pane holds the keyboard, and a slash line
# typed into the filter field is a filter, not a command.
shell-use press Escape
sleep 0.6
shell-use press Escape
sleep 0.6
shell-use submit "/timeline wombat:7"
# The parse happens BEFORE the pane is focused, so the refusal lands where the person typed —
# in the command output, with the usage line, not as an error inside a pane that opened anyway.
t an_unknown_filter_word_is_refused_with_the_usage \
  bash -c '
    for i in $(seq 1 30); do
      shell-use text | grep -q "usage: /timeline" && exit 0
      sleep 0.3
    done
    echo "the bad filter was never refused"
    exit 1
  '

# Esc is a TWO-STEP here (the ux1 rule): a typed editor clears first, and only an empty one lets
# the keyboard go back to the composer.
shell-use submit "/timeline"
sleep 1
shell-use type "agent:sol"
sleep 1
shell-use press Escape
sleep 0.8
shell-use press Escape
sleep 0.6
shell-use type "zz"
t esc_clears_the_editor_and_then_gives_the_keyboard_back \
  bash -c '
    for i in $(seq 1 20); do
      if shell-use text | tail -1 | grep -q "zz"; then
        shell-use text | grep -qE "filter \[agent:sol" && { echo "the editor kept the cleared text"; exit 1; }
        exit 0
      fi
      sleep 0.3
    done
    echo "the keys after two Escapes never reached the composer"
    exit 1
  '
for i in 1 2; do shell-use press Backspace; done

# A click on a row is the "go there" gesture: `on_click` answers a `FocusRequest` naming the row's
# step AND the Main pane, so the keyboard leaves the timeline for the transcript. That handover is
# the part a person feels, and it is what this asserts on the real screen.
shell-use submit "/timeline"
sleep 1
shell-use mouse click --on-text "thought/text" >/dev/null
sleep 0.8
shell-use type "qq"
t clicking_a_row_focuses_that_agent_and_step \
  bash -c '
    for i in $(seq 1 20); do
      if shell-use text | tail -1 | grep -q "qq"; then
        shell-use text | grep -qE "filter \[qq" && { echo "the click left the keyboard in the filter field"; exit 1; }
        exit 0
      fi
      sleep 0.3
    done
    echo "the keys after the click never reached the composer"
    exit 1
  '
for i in 1 2; do shell-use press Backspace; done

# `collapse_rows: 24` is a `SlotSize::Responsive` breakpoint, not a toggle (D-C2): under it the
# pane costs the Aux band NOTHING, and over it it comes back with no patch and no restart.
t the_pane_collapses_below_its_breakpoint \
  bash -c '
    shell-use resize 120 20 >/dev/null
    shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true
    for i in $(seq 1 20); do
      shell-use text | grep -qF -- "filter [" || break
      sleep 0.4
    done
    shell-use text | grep -qF -- "filter [" && { echo "the timeline is still laid out at 20 rows"; exit 1; }
    shell-use resize 120 40 >/dev/null
    shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true
    for i in $(seq 1 20); do
      shell-use text | grep -qF -- "filter [" && exit 0
      sleep 0.4
    done
    echo "the timeline did not come back at 40 rows"
    exit 1
  '

status_is_there() { shell-use text | grep -q "bough " || { echo "no status line"; exit 1; }; }
export -f status_is_there
t_size the_status_line_survives_every_size status_is_there

tui_quit
