#!/usr/bin/env bash
# Phase c, §8 — the drift dashboard pane.
#
# The dashboard's numbers are drift-watch's own signals; the verdicts and the arming rule are
# proven offline in the crate. What this script owns is the SURFACE: `/driftboard` opens the
# board, every agent has a row with a verdict, the reset is REACHABLE from the board and is a
# TWO-PRESS act (one `r` arms and says so; the second fires), and a resize keeps the status line.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && {
  skip_all "the drift board is a lens on scripted signals" \
  the_command_opens_the_drift_board \
  the_header_counts_every_agent \
  the_first_r_arms_the_reset_on_a_row_with_a_verdict \
  the_second_r_dispatches_the_reset_command \
  the_status_line_survives_every_size
  exit 0
}

# The pane is a CATALOG row in no bundle (the `tui-probe` precedent): this script mounts it, and
# only it, so the Aux band is not split three ways.
ROW="$(add_row drift <<'YML'

- id: tui.drift
  plugin: tui-drift
  config:
    height: 10
    collapse_rows: 24
    min_rows: 3
    max_rows: 30
    agents_shown: 12
    refresh_ms: 1000
    bar_cols: 12
    arm_ms: 3000
YML
)"

tui_open
tui_start "$ROW"

shell-use submit "say hello"
shell-use wait idle --timeout 30000

shell-use submit "/driftboard"
t the_command_opens_the_drift_board \
  bash -c '
    for i in $(seq 1 40); do
      shell-use text | grep -qE "drift · [0-9]+ agents" && exit 0
      sleep 0.3
    done
    echo "no drift header on screen"
    exit 1
  '

t the_header_counts_every_agent \
  bash -c '
    shell-use text | grep -qE "drift · 2 agents" || { echo "the header does not count both lanes"; exit 1; }
    shell-use text | grep -qE "too few samples|flagged" || { echo "the header carries no verdict count"; exit 1; }
    exit 0
  '

# The reset is a TWO-PRESS act (D-C5): a single `r` on a dashboard row must never throw a lane`s
# baseline away by itself.
shell-use press r
t the_first_r_arms_the_reset_on_a_row_with_a_verdict \
  bash -c '
    for i in $(seq 1 40); do
      shell-use text | grep -qE "press r again to reset (sol|terra)" && break
      sleep 0.3
    done
    shell-use text | grep -qE "press r again to reset (sol|terra)" || { echo "the first r said nothing about being armed"; exit 1; }
    # The armed row is a dashboard row: a sample count, a verdict and the reset it offers.
    shell-use text | grep -qE "(sol|terra) +n=[0-9]+" || { echo "no agent row with a sample count"; exit 1; }
    shell-use text | grep -qE "too-few-samples|steady|flagged|watch" || { echo "no verdict on the row"; exit 1; }
    shell-use text | grep -q "reset" || { echo "the row offers no reset"; exit 1; }
    exit 0
  '

shell-use press r
t the_second_r_dispatches_the_reset_command \
  bash -c '
    for i in $(seq 1 30); do
      # `/reset <agent>` is drift-watch`s own command; the board dispatches it and never resets
      # anything itself. Either the command answered or it said it does not know the word — both
      # are the board having DISPATCHED, which is the bullet.
      shell-use text | grep -qE "^reset: (sol|terra)" && exit 0
      sleep 0.3
    done
    echo "the second r dispatched nothing"
    exit 1
  '

# The reset`s own output is on screen; Esc dismisses it (the ux1 rule) before the resize walk.
shell-use press Escape
sleep 0.6
shell-use press Escape
sleep 0.6

status_is_there() { shell-use text | grep -q "bough " || { echo "no status line"; exit 1; }; }
export -f status_is_there
t_size the_status_line_survives_every_size status_is_there

tui_quit
