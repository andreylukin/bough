#!/usr/bin/env bash
# Phase c, §11 "Digging" — the byte-exact projection preview pane.
#
# The claim the pane makes is a BYTE claim, and bytes are asserted in
# `crates/bough/tests/preview_bytes.rs`, not here. What this script owns is the SURFACE: the
# command opens the pane, the pane names the agent and the high-water it assembled at, `t` moves
# between the two modes, Esc dismisses it, and a resize never leaves the status line behind.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && {
  skip_all "the preview pane is a lens on a scripted trajectory" \
  the_command_opens_the_preview_pane \
  the_header_names_the_agent_and_the_high_water \
  t_moves_between_head_and_anchored \
  esc_gives_the_keyboard_back_to_the_composer \
  the_status_line_survives_every_size
  exit 0
}

# The pane is a CATALOG row in no bundle (the `tui-probe` precedent): this script mounts it, and
# only it, so the Aux band is not split three ways.
ROW="$(add_row preview <<'YML'

- id: tui.preview
  plugin: tui-preview
  config:
    height: 12
    collapse_rows: 24
    min_rows: 4
    max_rows: 40
    refresh_ms: 150
    max_chars: 200000
YML
)"

tui_open
tui_start "$ROW"

# A turn first: a preview of an empty trajectory would assert nothing about the seam.
shell-use submit "say hello"
shell-use wait idle --timeout 30000

shell-use submit "/preview"
t the_command_opens_the_preview_pane \
  see "preview" --timeout 20000

t the_header_names_the_agent_and_the_high_water \
  bash -c '
    for i in $(seq 1 30); do
      shell-use text | grep -qE "preview .* as_of [0-9]+" && exit 0
      sleep 0.3
    done
    echo "no preview header with an as_of on screen"
    exit 1
  '

# The mode word is the honesty of the pane: `head` states its `+N preface rows` caveat, `anchored`
# is the mode the byte test asserts in (D-C1).
mode_word() { shell-use text | grep -oE "preview .* (head|anchored) as_of" | grep -oE "head|anchored" | head -1; }
export -f mode_word

before_mode="$(mode_word)"
shell-use press t
t t_moves_between_head_and_anchored \
  bash -c '
    for i in $(seq 1 30); do
      now="$(mode_word)"
      [ -n "$now" ] && [ "$now" != "'"$before_mode"'" ] && exit 0
      sleep 0.3
    done
    echo "the mode word never moved off '"$before_mode"'"
    exit 1
  '

shell-use press Escape
sleep 0.6
shell-use type "zz"
# The preview is a BAND pane, not an overlay: ux1's Esc rule hands the keyboard back, and the
# pane's own `on_key` says `Dismiss` for exactly that reason. Asserting "the pane vanished" would
# assert a shell behaviour this row does not have.
t esc_gives_the_keyboard_back_to_the_composer \
  bash -c '
    for i in $(seq 1 20); do
      shell-use text | tail -1 | grep -q "zz" && exit 0
      sleep 0.3
    done
    echo "the keys after Esc never reached the composer"
    exit 1
  '
for i in 1 2; do shell-use press Backspace; done

# The pane is `Responsive`: under its `collapse_rows` it costs nothing, and the status line — the
# one row phase ux1 says is always there — must be on screen at every size.
status_is_there() { shell-use text | grep -q "bough " || { echo "no status line"; exit 1; }; }
export -f status_is_there
t_size the_status_line_survives_every_size status_is_there

tui_quit
