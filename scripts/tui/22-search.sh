#!/usr/bin/env bash
# V7 — search (phase ux1 §2.7). M11: the search pane indexed the RAW LEDGER, so a persona looking
# for a word they had read on screen got rows of `request/header  {"as_of":53,…` and no way to get
# anywhere from them. The fix indexes the RENDERED conversation, envelope steps produce no entry at
# all, a hit is a snippet with the match highlighted and an `n of N` counter, and Enter or a click
# actually moves the transcript.
#
# `04-search.sh` still owns the Phase 3 behaviour (the pane opens, the query filters, the row
# disabled by patch degrades to a notice). This script owns what the audit asked for on top.
source "$(dirname "$0")/lib.sh"

# The live half does not run this script. Every bullet it carries is named here, so the
# skip COUNT matches the count the replay half prints (a whole-script skip printing one
# `ok` line for ten assertions is the dishonesty `skip` exists to avoid).
[ -n "$BOUGH_LIVE" ] && {
  skip_all "search indexes a scripted transcript" \
  the_search_pane_took_the_keyboard \
  hits_are_snippets_with_a_highlight_and_a_count \
  the_match_is_highlighted \
  no_hit_row_contains_a_brace \
  n_and_N_step_through_the_hits \
  enter_moves_the_transcript_to_the_hit \
  click_moves_the_transcript_to_the_hit \
  esc_clears_the_query_and_the_hits
  exit 0
}

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/scroll.patch.yml"

shell-use submit "fill the trajectory"
wait_for "trajectory line"
shell-use submit "one more turn"
wait_for "second turn line 20"

shell-use press "Ctrl+f"
t the_search_pane_took_the_keyboard \
  see "search" --timeout 20000

shell-use type "trajectory line 4"
sleep 1.5

t hits_are_snippets_with_a_highlight_and_a_count \
  bash -c '
    # The snippet: a hit row carries the matched text WITH context around it, not a bare step id.
    see "trajectory line 4" --timeout 10000 || { echo "no hit row carries the matched text"; exit 1; }
    # The counter, in the shape §2.7 names.
    shell-use text | grep -qE "[0-9]+ of [0-9]+" || { echo "no n-of-N counter on screen"; exit 1; }
  '

t the_match_is_highlighted \
  bash -c '
    # The colour half. The match bytes carry the theme`s `sel_bg`; asserted through the cells dump
    # because a highlight asserted as TEXT would pass on a screen that draws none.
    # Below the FIELD, always: the same words are in the trajectory beside the pane, and the first
    # row carrying them is that copy, which is not highlighted and never will be.
    shell-use text | python3 -c "
import sys
rows = sys.stdin.read().split(chr(10))
field = next((i for i, r in enumerate(rows) if \"search [\" in r), None)
if field is None:
    sys.exit(1)
for y in range(field + 1, len(rows)):
    if \"trajectory line 4\" in rows[y]:
        print(rows[y].index(\"trajectory line 4\"), y)
        break
else:
    sys.exit(1)
" > "$HOME_DIR/hit.pos" || { echo "no hit row on screen"; exit 1; }
    read x y < "$HOME_DIR/hit.pos"
    cells_have "$x" "$y" 17 1 bg "#2d3f60"
  '

t no_hit_row_contains_a_brace \
  bash -c '
    # The whole point of indexing rendered text: no raw ledger JSON reaches the pane.
    shell-use text | grep -q "request/header" && { echo "a raw envelope step is in the results"; exit 1; }
    shell-use text | grep -q "{\"" && { echo "raw JSON is in the results"; exit 1; }
    shell-use text | grep -q "as_of" && { echo "an envelope field is in the results"; exit 1; }
    exit 0
  '

# --- n / N step through the hits and the counter moves with them. -----------------------------
counter() { shell-use text | grep -oE "[0-9]+ of [0-9]+" | head -1; }
export -f counter

first_counter="$(counter)"
shell-use press "Ctrl+n"
sleep 0.8
# `Ctrl+n` / `Ctrl+Shift+n`, not bare `n`/`N`: the field is a TEXT input and a query containing
# the letter n must stay typable (root cause (c), text is never destroyed). Up/Down do the same.
t n_and_N_step_through_the_hits \
  bash -c '
    second="$(counter)"
    [ -n "$second" ] || { echo "the counter vanished"; exit 1; }
    [ "$second" != "'"$first_counter"'" ] || { echo "^n did not move the counter off '"$first_counter"'"; exit 1; }
    # Backwards with `Up`, not `Ctrl+Shift+n`: a terminal without the kitty keyboard protocol
    # cannot tell `Ctrl+Shift+n` from `Ctrl+n` at all — it sends the same byte — so the shifted
    # chord is unassertable here. `Up`/`Down` are bound to the same two steps for that reason.
    shell-use press Up
    sleep 0.8
    back="$(counter)"
    [ "$back" = "'"$first_counter"'" ] || { echo "Up did not come back to '"$first_counter"' (got $back)"; exit 1; }
  '

# --- Enter jumps, and the TRANSCRIPT moves — asserted on the visible row, not on state. -------
top_line() { shell-use text | sed -n '3p' | cut -c35-; }
export -f top_line

before_jump="$(top_line)"
shell-use press Enter
t enter_moves_the_transcript_to_the_hit \
  bash -c '
    for i in $(seq 1 30); do
      now="$(top_line)"
      [ "$now" != "'"$before_jump"'" ] && exit 0
      sleep 0.2
    done
    echo "the transcript never moved after Enter on a hit"
    exit 1
  '

# --- A click on a hit jumps too. --------------------------------------------------------------
# Esc first: it empties the field and hands the keyboard back, so the new query is a query and not
# a suffix of the last one. TWICE, because Esc dismisses one thing at a time and a notice from the
# jump above may be holding the first press.
shell-use press Escape
sleep 0.4
shell-use press Escape
sleep 0.4
shell-use press "Ctrl+f"
shell-use type "trajectory line 52"
sleep 1.5
# Away from the anchor first: both queries match lines of the SAME step (one long answer), so a
# viewport already sitting on that step could not move and the bullet would be unassertable.
shell-use press End
sleep 0.8
before_click="$(top_line)"
# Click the HIT ROW, which is the row under the field — the same words are in the transcript
# beside the pane, and clicking that copy proves nothing about the search.
hit_xy="$(shell-use text | python3 -c "
import sys
rows = sys.stdin.read().split(chr(10))
field = next((i for i, r in enumerate(rows) if 'search [' in r), None)
if field is None:
    sys.exit(1)
for y in range(field + 1, len(rows)):
    if 'trajectory line 52' in rows[y]:
        print(rows[y].index('trajectory line 52'), y)
        break
else:
    sys.exit(1)
")" || { echo "not ok - the hit row for the click never appeared"; exit 1; }
read hx hy <<< "$hit_xy"
shell-use mouse click "$hx" "$hy"
t click_moves_the_transcript_to_the_hit \
  bash -c '
    for i in $(seq 1 30); do
      now="$(top_line)"
      [ "$now" != "'"$before_click"'" ] && exit 0
      sleep 0.2
    done
    echo "the transcript never moved after a click on a hit"
    exit 1
  '

# --- Esc clears query, hits AND rows (minor 30). ----------------------------------------------
shell-use press "Ctrl+f"
shell-use type "trajectory"
sleep 1.5
# Esc is the shell's "give the composer the keyboard back" binding; the pane clears on losing
# focus, which is the same thing from the reader's side.
shell-use press Escape
sleep 0.8
t esc_clears_the_query_and_the_hits \
  bash -c '
    shell-use text | grep -qE "[0-9]+ of [0-9]+" && { echo "the counter survived Esc"; exit 1; }
    shell-use press "Ctrl+f"
    sleep 1
    shell-use text | grep -qE "[0-9]+ of [0-9]+" && { echo "the old hits came back with the pane"; exit 1; }
    exit 0
  '
shell-use press Escape

tui_quit
