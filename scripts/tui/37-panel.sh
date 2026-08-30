#!/usr/bin/env bash
# §11 "The panel" — the tabbed surface over the running composition.
#
# The toggle's WRITE PATH (the ui layer, the recompose, the same-fiber re-enable) is asserted in
# `crates/bough/tests/panel_swap.rs`, not here. What this script owns is the SURFACE: `^t` opens
# the panel, the config tab shows the composed rows, `]` reaches connectors, `/model` opens its
# tab, Esc hands the keyboard back, and the status line survives every size. The row ships IN
# `bough-tui-app` (closed it costs zero rows), so nothing is mounted by patch here.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && {
  skip_all "the panel is a lens on the composed tree, not on a model" \
  ctrl_t_opens_the_panel_on_the_config_tab \
  the_config_tab_lists_the_composed_rows \
  bracket_switches_to_connectors \
  the_model_command_opens_its_tab \
  esc_closes_and_the_keys_return_to_the_composer \
  the_status_line_survives_every_size
  exit 0
}

tui_open
tui_start

shell-use press "Ctrl+t"
t ctrl_t_opens_the_panel_on_the_config_tab \
  bash -c '
    for i in $(seq 1 30); do
      shell-use text | grep -q "connectors" && shell-use text | grep -q "tree " && exit 0
      sleep 0.3
    done
    echo "no panel tab bar on screen after ^t"
    exit 1
  '

# The joined rows: a shipped row id with its state word beside it, from the SAME snapshot the
# boot report reads. `llm.anthropic` sits in the tree's first screenful at the shipped panel
# height (`tui.focus` is real too, but below the viewport, which is what the cursor is for).
t the_config_tab_lists_the_composed_rows \
  bash -c '
    for i in $(seq 1 30); do
      shell-use text | grep -q "llm.anthropic" && shell-use text | grep -q "active" && exit 0
      sleep 0.3
    done
    echo "the config tab never showed the composed rows"
    exit 1
  '

shell-use press ]
t bracket_switches_to_connectors \
  bash -c '
    for i in $(seq 1 20); do
      shell-use text | grep -q "collectors" && exit 0
      sleep 0.3
    done
    echo "] never reached the connectors tab"
    exit 1
  '

shell-use submit "/model"
t the_model_command_opens_its_tab \
  bash -c '
    for i in $(seq 1 20); do
      shell-use text | grep -q "unattended" && exit 0
      sleep 0.3
    done
    echo "/model never showed the policy readout"
    exit 1
  '

shell-use press Escape
sleep 0.6
shell-use type "zz"
t esc_closes_and_the_keys_return_to_the_composer \
  bash -c '
    for i in $(seq 1 20); do
      shell-use text | tail -1 | grep -q "zz" && exit 0
      sleep 0.3
    done
    echo "the keys after Esc never reached the composer"
    exit 1
  '
for i in 1 2; do shell-use press Backspace; done

status_is_there() { shell-use text | grep -q "bough " || { echo "no status line"; exit 1; }; }
export -f status_is_there
t_size the_status_line_survives_every_size status_is_there

tui_quit
