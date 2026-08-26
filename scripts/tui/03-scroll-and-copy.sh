#!/usr/bin/env bash
# V3 — scrolling, scroll STABILITY while new steps stream, and a drag selection copied out over
# OSC52. The copy assertion decodes the base64 out of the session recording and compares it with
# the cells that were selected: the sequence has to carry the real text, not merely exist.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip the_wheel_scrolls_the_trajectory "scroll geometry needs the replay transcript"; exit 0; }

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/scroll.patch.yml"

shell-use submit "fill the trajectory"
shell-use wait idle --timeout 30000

# The top visible line OF THE TRAJECTORY. Column 35 onward, because a whole screen row also spans
# the rail — whose about-line legitimately changes when a new turn starts, which made
# `the_viewport_does_not_move_while_new_steps_stream` fail on the pane it was not asserting about.
top_line() { shell-use text | sed -n '3p' | cut -c35-; }
export -f top_line

# `top_changed_from <line>`: the top visible trajectory line becomes something other than <line>.
#
# Polled, not read once: the SGR mouse report and the key press are handed to the event loop and
# the redraw is a later frame, so a single read raced the repaint and reported the pre-scroll line
# as if the scroll had not happened.
top_changed_from() {
  local was="$1" i now
  for i in $(seq 1 30); do
    now="$(top_line)"
    [ "$now" != "$was" ] && return 0
    sleep 0.2
  done
  echo "the top trajectory line is still [$was] after 6s"
  return 1
}
export -f top_changed_from

before="$(top_line)"
wheel 60 10 up 5
t the_wheel_scrolls_the_trajectory \
  bash -c "top_changed_from \"$before\""

# Back to the bottom before the key half, so PageUp is measured with room above it. The wheel
# burst can reach the top of the trajectory on its own, and a PageUp that is already clamped
# there correctly does nothing — which reads as a failure without being one.
wheel 60 10 down 20
sleep 0.5
pinned="$(top_line)"
shell-use press PageUp
t page_up_and_arrow_keys_scroll_the_trajectory \
  bash -c "top_changed_from \"$pinned\""

# A second replayed turn starts while the viewport is scrolled up: the top visible line must be
# byte-identical before and after.
held="$(top_line)"
shell-use submit "one more turn"
shell-use wait idle --timeout 30000
t the_viewport_does_not_move_while_new_steps_stream \
  bash -c "[ \"\$(top_line)\" = \"$held\" ]"

# Home/End/arrows move the COMPOSER's cursor while the composer holds the keyboard — only the
# wheel and PageUp/PageDown are routed past it (`run.rs::on_key`). So this bullet takes keyboard
# focus the way a user does: it clicks the trajectory first.
shell-use mouse click 60 10
shell-use wait idle --timeout 5000 >/dev/null 2>&1 || true
shell-use press End
t end_re_arms_follow_and_jumps_to_the_bottom \
  bash -c "top_changed_from \"$held\""

select_drag 40 5 70 5
t a_drag_selection_is_highlighted \
  expect_selected 40 5 30 "#2d3f60"

t the_release_emits_an_osc52_sequence_carrying_the_selected_text \
  bash -c '
    # The selected cells, read off the screen: row 5 (0-based) is text line 6, and the drag ran
    # from column 40 to 70 (0-based) — trailing padding trimmed, which is what the copy path does.
    want="$(shell-use text | sed -n "6p" | cut -c41-71 | sed "s/[[:space:]]*$//")"
    [ -n "$want" ] || { echo "the selected run is blank; the fixture did not paint there"; exit 1; }
    # The sequence is written on the release, and reaches the recording a frame later: poll.
    for i in $(seq 1 25); do
      rec="$(shell-use get-recording)"
      payload="$(printf "%s" "$rec" | grep -o "\\u001b]52;c;[A-Za-z0-9+/=]*" | tail -1 | sed "s/.*;c;//")"
      [ -n "$payload" ] && break
      sleep 0.2
    done
    [ -n "$payload" ] || { echo "no OSC52 sequence in the recording"; exit 1; }
    text="$(printf "%s" "$payload" | base64 --decode 2>/dev/null)"
    [ "$text" = "$want" ] || { echo "OSC52 carried [$text], the selected cells are [$want]"; exit 1; }
  '

tui_quit
