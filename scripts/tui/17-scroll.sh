#!/usr/bin/env bash
# V2 — scroll (phase ux1 §2.2). Blocker 2 and major 23 are the same bug seen from two personas: the
# scroll keys were routed through whatever had the keyboard, so the walks contradicted each other
# about whether PageUp worked. The fix is that PageUp/PageDown/Home/End reach `tui.transcript_pane`
# from EVERY focus, and the wheel goes to the pane under the pointer without moving focus at all.
#
# Every bullet here is measured on the TOP VISIBLE TRAJECTORY LINE, the way `03-scroll-and-copy.sh`
# measures: a whole screen row also spans the rail, whose about-line legitimately changes on its own.
source "$(dirname "$0")/lib.sh"

# The live half does not run this script. Every bullet it carries is named here, so the
# skip COUNT matches the count the replay half prints (a whole-script skip printing one
# `ok` line for ten assertions is the dishonesty `skip` exists to avoid).
[ -n "$BOUGH_LIVE" ] && {
  skip_all "scroll geometry needs the replay transcript" \
  the_trajectory_is_long_enough_to_scroll \
  scroll_keys_work_from_the_composer \
  scroll_keys_work_from_the_focus_pane \
  scroll_keys_work_from_the_search_pane \
  the_wheel_scrolls_the_transcript \
  the_tail_follows_a_live_answer \
  an_anchored_viewport_does_not_move_while_streaming \
  scrolled_up_shows_the_new_badge \
  end_returns_to_the_latest_row
  exit 0
}

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/scroll.patch.yml"

shell-use submit "fill the trajectory"
shell-use wait idle --timeout 30000
t the_trajectory_is_long_enough_to_scroll see "trajectory line" --timeout 20000

top_line() { shell-use text | sed -n '3p' | cut -c35-; }
export -f top_line

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

top_stayed() {
  local was="$1" i
  for i in $(seq 1 10); do
    sleep 0.3
    [ "$(top_line)" = "$was" ] || { echo "the top trajectory line moved from [$was] to [$(top_line)]"; return 1; }
  done
  return 0
}
export -f top_stayed

# --- The three focuses. The SAME key, from each of them. ---------------------------------------
#
# This is the bullet the audit's contradiction demanded: not "PageUp works" but "PageUp works no
# matter where the keyboard is". Each leg re-arms the tail with End first, so each is measured with
# room above it — a PageUp already clamped at the top correctly does nothing.

shell-use press End
sleep 0.5
from_composer="$(top_line)"
shell-use press PageUp
t scroll_keys_work_from_the_composer \
  bash -c "top_changed_from \"$from_composer\""

shell-use press End
sleep 0.5
shell-use press Tab
shell-use wait idle --timeout 5000 >/dev/null 2>&1 || true
from_pane="$(top_line)"
shell-use press PageUp
t scroll_keys_work_from_the_focus_pane \
  bash -c "top_changed_from \"$from_pane\""

shell-use press End
sleep 0.5
shell-use keys "Ctrl+f"
shell-use wait idle --timeout 5000 >/dev/null 2>&1 || true
from_search="$(top_line)"
shell-use press PageUp
t scroll_keys_work_from_the_search_pane \
  bash -c "top_changed_from \"$from_search\""
shell-use press Escape

# --- The wheel, at a cell inside the transcript. -----------------------------------------------
shell-use press End
sleep 0.5
before_wheel="$(top_line)"
wheel 60 10 up 5
t the_wheel_scrolls_the_transcript \
  bash -c "top_changed_from \"$before_wheel\""

# --- Follow at the tail. A live answer arrives and the view goes with it. ----------------------
shell-use press End
sleep 0.5
tail_before="$(top_line)"
shell-use submit "one more turn"
shell-use wait idle --timeout 30000
t the_tail_follows_a_live_answer \
  bash -c "top_changed_from \"$tail_before\" && see 'second turn line 20' --timeout 20000"

# --- Anchored: scrolled up, the viewport does not move and a badge counts what arrived. --------
shell-use press PageUp
shell-use press PageUp
sleep 0.5
anchored="$(top_line)"
shell-use submit "and another"
sleep 2

t an_anchored_viewport_does_not_move_while_streaming \
  bash -c "top_stayed \"$anchored\""

shell-use wait idle --timeout 30000
t scrolled_up_shows_the_new_badge \
  see "new" --timeout 20000

# --- End goes back to the latest row and clears the badge. -------------------------------------
shell-use press End
t end_returns_to_the_latest_row \
  bash -c "top_changed_from \"$anchored\" && see 'second turn line 20' --timeout 20000"

tui_quit
