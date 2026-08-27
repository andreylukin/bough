#!/usr/bin/env bash
# V5 — the frame (phase ux1 §2.5). Four of the audit's findings are one shape: the chrome painted
# over the content and told the user nothing. The rail was 34 columns at 80 and at 200 (M13), rail
# and transcript shared baselines (M9), the strip named neither model nor cwd nor cost (M24), and a
# resize injected blank lines (nit 39).
#
# The bullets here are geometric, so they are asserted at three sizes rather than at the boot one.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip the_status_line_names_the_six_things "the frame is layout, not a model"; exit 0; }

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/scroll.patch.yml"

shell-use submit "fill the trajectory"
shell-use wait idle --timeout 30000

# --- The status line says the six things §2.5 lists. -------------------------------------------
#
# At a WIDE terminal, deliberately: the six fields plus the key hints do not fit on a narrow row,
# and what happens then is the drop chain, which the crate's own tests pin at 200/120/80/40. This
# bullet is about the line SAYING the six things when there is room to.
shell-use resize 160 40
shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true
sleep 0.6
#
# `%` and `$` are asserted as the SHAPES they are: the exact number moves with every turn, and a
# bullet that pinned it would be a bullet about arithmetic rather than about the line existing.
t the_status_line_names_the_six_things \
  bash -c '
    see "bough" --timeout 20000 || exit 1
    see "'"$HOME_DIR"'/work" --timeout 8000 || see "work" --timeout 8000 || { echo "no cwd on the status line"; exit 1; }
    see "haiku" --timeout 8000 || { echo "no model on the status line"; exit 1; }
    see "%" --timeout 8000 || { echo "no context percentage on the status line"; exit 1; }
    shell-use text | grep -q "[$]\|—" || { echo "no cost field on the status line"; exit 1; }
    see "help" --timeout 8000 || { echo "no key hints on the status line"; exit 1; }
  '

t the_status_line_is_exactly_one_row \
  bash -c '
    n="$(shell-use text | grep -c "esc")"
    [ "${n:-0}" -ge 1 ] || { echo "the interrupt hint is on no row"; exit 1; }
    [ "${n:-0}" -le 2 ] || { echo "the status hints are on $n rows; the line is one row"; exit 1; }
  '

# --- 80x24: the rail collapses, and nothing shares a baseline with the transcript. -------------
shell-use resize 80 24
shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true
sleep 0.6

t at_80x24_the_rail_is_gone_and_no_row_carries_two_runs \
  bash -c '
    # The rail collapsed: its about-line and its agent names are not on screen at all.
    shell-use text | grep -q "trajectory line" || { echo "the transcript is not drawn at 80x24"; exit 1; }
    # M9 in its measurable form: no row of the transcript carries a run of text, a run of blanks
    # wide enough to be a gutter, and then a SECOND run — which is what a rail sharing a baseline
    # with the conversation looks like.
    shell-use text | python3 -c "
import re, sys
bad = []
for y, row in enumerate(sys.stdin.read().split(chr(10))):
    if \"trajectory line\" not in row:
        continue
    if re.search(r\"\\S {4,}\\S\", row):
        bad.append((y, row))
if bad:
    y, row = bad[0]
    sys.exit(\"row %d carries two runs: %r\" % (y, row))
"
  '

# --- 200x50: the prose measure is capped. ------------------------------------------------------
shell-use resize 200 50
shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true
sleep 0.6
shell-use submit "one more turn"
shell-use wait idle --timeout 30000

t at_200x50_the_measure_is_capped_at_ninety \
  bash -c '
    # `measure_cols` is 90 and the rail is clamped at `max_width` 40 plus a gutter: no rendered
    # transcript row may reach the right edge of a 200-column terminal.
    shell-use text | python3 -c "
import sys
worst = 0
for row in sys.stdin.read().split(chr(10)):
    if \"turn line\" in row or \"trajectory line\" in row:
        worst = max(worst, len(row.rstrip()))
if worst > 140:
    sys.exit(\"a transcript row is %d columns wide at 200: the measure is not capped\" % worst)
if worst == 0:
    sys.exit(\"no transcript row on screen at 200x50\")
"
  '

# --- Overlays dismiss with Esc, one at a time, and Esc on nothing is harmless. -----------------
shell-use resize 120 40
shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true
sleep 0.5

t esc_dismisses_help_then_search_then_nothing \
  bash -c '
    shell-use submit "/help"
    see "help" --timeout 10000 || exit 1
    shell-use press Escape
    sleep 0.6
    shell-use keys "Ctrl+f"
    # The FIELD, not the word: `^f search` is a key hint on the status line at every moment, so a
    # bullet that looked for "search" anywhere would pass with the pane shut and fail with it open.
    see "search [" --timeout 10000 || exit 1
    shell-use type "somequery"
    see "search [somequery" --timeout 8000 || { echo "the query never reached the field"; exit 1; }
    shell-use press Escape
    sleep 0.6
    # The search pane is a SLOT, not a floating overlay, so Esc empties it and hands the keyboard
    # back rather than making a registered pane vanish.
    see "search [somequery" --not --timeout 8000       || { echo "Esc did not clear the search query"; exit 1; }
    # Esc with nothing open must not clear a draft (§2.3) and must not exit.
    shell-use type "a draft after the overlays"
    shell-use press Escape
    see "a draft after the overlays" --timeout 8000
  '
shell-use keys "Ctrl+u"

# --- The three-size walk: history re-wraps and nothing is injected. ---------------------------
rewrapped_cleanly() {
  local cols="$1"
  shell-use text | grep -q "turn line\|trajectory line" || { echo "the transcript vanished"; return 1; }
  no_blank_run 3 "line" || return 1
  # No row may exceed the terminal: a stored wrap from a WIDER size would.
  shell-use text | COLS="$cols" python3 -c '
import os, sys
cols = int(os.environ["COLS"])
for y, row in enumerate(sys.stdin.read().split("\n")):
    if len(row.rstrip()) > cols:
        sys.exit("row %d is %d columns wide in a %d-column terminal" % (y, len(row.rstrip()), cols))
'
}
export -f rewrapped_cleanly

t_size three_sizes_rewrap_with_no_blank_line_injected rewrapped_cleanly

tui_quit
