#!/usr/bin/env bash
# V1 — the focus model (phase ux1 §2.1). The audit's first blocker was that a click on the
# transcript silently moved the keyboard off the composer: the composer kept drawing, the cursor
# kept blinking, and everything typed after the click went nowhere. The rule this script pins is
# the one sentence of §2.1 — the composer always has the keyboard unless the user deliberately
# gave it away, and any printable key takes it back — plus the keyboard path to a tool row that
# blocker 6 found missing altogether.
#
# The tool-call transcript, because three of the seven bullets are about a ROW: only a fixture
# with real tool rows can have one focused, toggled or clicked.
source "$(dirname "$0")/lib.sh"

# The live half does not run this script. Every bullet it carries is named here, so the
# skip COUNT matches the count the replay half prints (a whole-script skip printing one
# `ok` line for ten assertions is the dishonesty `skip` exists to avoid).
[ -n "$BOUGH_LIVE" ] && {
  skip_all "the focus model is composition and keys, not a model" \
  the_tool_rows_are_on_screen \
  the_click_did_not_steal_the_keyboard \
  click_then_type_still_sends \
  tab_paints_a_focus_ring \
  arrows_move_a_visible_row_focus \
  enter_toggles_the_focused_row \
  enter_toggles_the_focused_row_back \
  space_toggles_the_focused_row \
  a_printable_key_snaps_focus_back_to_the_composer \
  the_first_click_expands_the_row_it_landed_on \
  click_toggles_the_row_it_landed_on \
  the_four_audit_paths_lose_nothing \
  no_ring_before_tab
  exit 0
}

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/tool-calls.patch.yml"

shell-use submit "run the scripted tools"
wait_for "bash" 30000
t the_tool_rows_are_on_screen see "bash" --timeout 20000

# --- B1: click the transcript, then type. The turn must start. ---------------------------------
shell-use mouse click --on-text "bash"
sleep 0.5
shell-use type "a message typed after a click"
t the_click_did_not_steal_the_keyboard \
  see "a message typed after a click" --timeout 10000
shell-use press Enter
t click_then_type_still_sends \
  see "the scripted tools ran" --timeout 30000

# --- B6: a VISIBLE roving row focus, moved by the arrows, toggled by Enter and Space. ----------
#
# `Tab` first, deliberately: §2.1 is explicit that the arrows belong to the COMPOSER while the
# composer has the keyboard, and that a pane only gets them once the user handed the keyboard over.
# A bullet that pressed Down without Tab would be asserting the opposite of the design.
# The ring: a BEFORE/AFTER pair on the one glyph that only the ring draws.
#
# This bullet used to count cells carrying `#7aa2f7` anywhere on a 120x40 screen and assert `> 0`,
# with no baseline. That colour is `Theme::dark.accent`, which the status line's product name and
# every user-message label already paint on every frame — and the walk sends a message before
# pressing Tab. The assertion was true before Tab was ever pressed. (There was also no ring: the
# `PaneView::is_focused` the shell writes was read by no pane in the tree.)
#
# `▎` (U+258E) is the ring column `tui-focus` reserves and paints in `theme.accent` only when it
# holds the keyboard. It is not `▌` (the row-focus marker) and not `▏` (the search field's caret),
# so its presence is the ring and nothing else.
t no_ring_before_tab \
  bash -c '[ "$(shell-use text | grep -c "▎" || true)" -eq 0 ]'

# Tab cycles over EVERY focusable stop, and only the transcript draws a ring, so the walk keeps
# Tabbing until the keyboard reaches it. Which stop of the ring that is, is the layout's business.
t tab_paints_a_focus_ring \
  bash -c '
    for i in $(seq 1 6); do
      shell-use press Tab >/dev/null
      sleep 0.5
      [ "$(shell-use text | grep -c "▎" || true)" -gt 0 ] && exit 0
    done
    echo "no ring column after cycling the whole focus ring with Tab"
    exit 1
  '

# The focused row is DRAWN as focused, not merely tracked. `▌` is the marker the transcript
# paints beside the focused row (`tui-focus::rowfocus::focus_marker`, §2.1: "a roving, VISIBLE
# row focus").
# Which stop of the ring the transcript is at is the layout's business, not this bullet's, so the
# walk keeps Tabbing until the arrows land somewhere that paints a focused row.
reach_a_focused_row() {
  local i
  # First find the stop where the arrows paint a focused row at all…
  for i in $(seq 1 6); do
    shell-use press Up >/dev/null
    sleep 0.3
    if shell-use text | grep -q "▌"; then break; fi
    shell-use press Tab >/dev/null
    sleep 0.3
  done
  # …then walk the focus UP to the row this bullet names. A keyboard user arriving from the
  # composer lands on the NEWEST row (`RowFocus::moved` from `None`), so the older tool rows are
  # up from there. Where `read_file` sits is the fixture's business, so the walk is bounded
  # rather than counted.
  for i in $(seq 1 20); do
    if row_with "▌" "read_file" >/dev/null 2>&1; then return 0; fi
    shell-use press Up >/dev/null
    sleep 0.25
  done
  row_with "▌" "read_file"
}
export -f reach_a_focused_row
t arrows_move_a_visible_row_focus reach_a_focused_row

shell-use press Enter
t enter_toggles_the_focused_row \
  see "path:" --timeout 8000
shell-use press Enter
t enter_toggles_the_focused_row_back \
  see "path:" --not --timeout 8000

shell-use press Space
t space_toggles_the_focused_row \
  see "path:" --timeout 8000
shell-use press Space

# --- A printable key takes the keyboard back, with no Tab and no click. ------------------------
shell-use type "back to the composer"
t a_printable_key_snaps_focus_back_to_the_composer \
  see "back to the composer" --timeout 10000

# --- M26: a click toggles the row it LANDED on, expanded rows included. ------------------------
#
# The audit's repro is the hit-test origin bug: with one row already expanded, every later click
# toggled a row several lines above the one under the pointer. So this bullet expands one row by
# click, leaves it expanded, and then clicks a DIFFERENT row and requires BOTH bodies on screen.
shell-use keys "Ctrl+u"

# `click_until <text-to-click> <text-that-proves-it>`: a click aimed at a coordinate the driver
# read one frame ago can land on a row that has since moved — the transcript reflows under an
# expansion. A hand does not have that race (it aims at what it sees); a script does, so it
# re-aims and clicks again rather than reporting a hit-test bug that is not there.
click_until() {
  local target="$1" proof="$2" i
  for i in $(seq 1 5); do
    shell-use mouse click --on-text "$target" >/dev/null
    sleep 0.6
    if shell-use text | grep -qF "$proof"; then return 0; fi
  done
  echo "clicking '$target' never put '$proof' on screen"
  return 1
}
export -f click_until

t the_first_click_expands_the_row_it_landed_on \
  bash -c 'click_until "bash" "exit 0"'
# MERGE (track B -> ux1): the second half SCROLLS to the first body instead of demanding it still
# be on screen. The merged tree gives the column a `drafts` pane (`tui.drafts`, 30%), so the
# transcript viewport is shorter than it was when this bullet was written and two expanded tool
# bodies no longer both fit at the suite's size. The claim is unchanged and was never about
# pixels: the row the pointer landed on TOGGLED, and the row that was already open did NOT
# collapse — which is exactly the hit-test origin bug the audit found.
t click_toggles_the_row_it_landed_on \
  bash -c 'click_until "write_file" "+fn main() {" && see_anywhere "exit 0"'

# --- The whole point, restated as one bullet: no path through this script loses typed text. ----
#
# B1's click, B6's Tab-and-arrows, M23's PageUp and M26's second click, in sequence, with a draft
# held across all four. The draft has to still be there at the end.
shell-use type "the draft that survives everything"
shell-use mouse click --on-text "bash"
shell-use press Tab
shell-use press Down
shell-use press PageUp
shell-use mouse click --on-text "write_file"
t the_four_audit_paths_lose_nothing \
  see "the draft that survives everything" --timeout 10000

tui_quit
