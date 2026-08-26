#!/usr/bin/env bash
# V4 — the FTS pane: focus it, query it, read the hits, click one, and see a bad query reported
# inline instead of thrown.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip ctrl_f_focuses_the_search_pane "the search corpus is the replayed trajectory"; exit 0; }

tui_open
tui_start

shell-use submit "say the whole sentence please"
shell-use wait idle --timeout 30000
# WAIT FOR THE CORPUS, not merely for the screen to stop moving. `wait idle` returned before the
# replayed turn had landed in roughly one run in three, and a search over an empty ledger then
# failed the query bullet for a reason that has nothing to do with the search pane.
shell-use expect text --no-strict "and the rest of it" --timeout 30000 >/dev/null

shell-use keys "Ctrl+f"
# The pane's prompt NAMES itself, and its trailing `_` is the pane's focus cursor (`lines()` draws
# it only when `view.is_focused`). Asserting the cursor and not the bare word is what makes this a
# focus bullet: "search" alone is on screen whether or not Ctrl+F did anything.
t ctrl_f_focuses_the_search_pane \
  see "search / _" --timeout 5000

# The hit list, read as the SEARCH PANE's own rows rather than as text anywhere on screen. The
# trajectory beside it quotes the same words, so `see "fragment"` passed whether the query ran or
# not; these bullets poll for a row UNDER the `search /` prompt matching the shape `hit_line`
# paints: `<agent> s<seq> <kind>  <snippet>`.
hits_file="$HOME_DIR/hits.txt"
await_hits() {
  local i n
  for i in $(seq 1 40); do
    shell-use text > "$hits_file.raw"
    n="$(grep -n "search / " "$hits_file.raw" | head -1 | cut -d: -f1)"
    if [ -n "$n" ]; then
      sed -n "$((n + 1)),\$p" "$hits_file.raw" | grep -E "^ *[a-z]+ s[0-9]+ [a-z]+/[a-z]+ " > "$hits_file" || true
      [ -s "$hits_file" ] && return 0
    fi
    sleep 0.25
  done
  echo "no hit row of the shape '<agent> s<seq> <kind>' ever appeared under the search prompt"
  return 1
}

shell-use type "fragment"
t a_query_lists_hits_with_agent_and_step_id \
  await_hits
# Every hit names its OWN agent — the only agent in this profile with a trajectory is `sol`, and a
# rowless trajectory would render no name at all (`hit_line`).
t a_hit_names_the_agent_that_owns_it \
  bash -c 'grep -qE "^ *sol s[0-9]+ " "'"$hits_file"'" || { cat "'"$hits_file"'"; exit 1; }'

# Click the hit that names the answer STEP (`thought/text`), and assert the trajectory row for that
# step is the flashed one: `tui-focus` paints an anchored row in `theme.accent` (#7aa2f7 in the
# dark theme), everything else in `theme.fg`. Read through `shell-use cells`, per the plan.
t clicking_a_hit_focuses_that_step_in_the_trajectory \
  bash -c '
    set -e
    shell-use text > "$HOME_DIR/pre.txt"
    y="$(grep -n "s[0-9]* thought/text" "$HOME_DIR/pre.txt" | head -1 | cut -d: -f1)"
    [ -n "$y" ] || { echo "no thought/text hit row on screen"; exit 1; }
    shell-use mouse click 40 "$((y - 1))" >/dev/null
    for i in $(seq 1 25); do
      shell-use text > "$HOME_DIR/post.txt"
      ty="$(grep -n "the first fragment and the rest of it" "$HOME_DIR/post.txt" | head -1 | cut -d: -f1)"
      if [ -n "$ty" ]; then
        fg="$(shell-use cells 34 "$((ty - 1))" 3 1 --json | python3 -c "
import json,sys
print(\",\".join(c[\"fg\"] for c in json.load(sys.stdin)[\"data\"][\"cells\"]))
")"
        [ "$fg" = "#7aa2f7,#7aa2f7,#7aa2f7" ] && exit 0
      fi
      sleep 0.2
    done
    echo "the trajectory row for the clicked step is $fg, not the accent #7aa2f7"
    exit 1
  '

shell-use keys "Ctrl+f"
# Clear the previous query first: without it the query is `fragment"unbalanced` — still a bad
# query, but a bad one for the wrong reason. Backspaces and not Escape, because Escape is the
# shell's "give the composer the keyboard back" binding (`run.rs::keymap`) and never reaches the
# pane at all.
shell-use press Backspace Backspace Backspace Backspace Backspace Backspace Backspace Backspace
shell-use type '"unbalanced'
# NOT `a_bad_query_reports_inline_and_clears_the_list`, which the plan named and which cannot
# happen: `ledger-sqlite::match_expr` tokenises a query to alphanumeric runs and quotes each one
# (P1-D19), so `"unbalanced` reaches FTS5 as `"unbalanced"` — a VALID query with no hits. No input
# typed into this pane can produce an FTS5 syntax error. The `SearchState::apply(gen, Err(msg))`
# transition that renders `! <error>` inline is still covered, as a unit test in
# `plugins/tui-search/tests/search.rs`; what the SCREEN can prove is the other half of the bullet,
# that a query matching nothing clears the previous hits instead of leaving them there to lie.
t a_query_that_matches_nothing_clears_the_list \
  bash -c '
    for i in $(seq 1 25); do
      n="$(shell-use text | grep -n "search / " | head -1 | cut -d: -f1)"
      if [ -n "$n" ] && [ -z "$(shell-use text | sed -n "$((n + 1))p" | tr -d " ")" ]; then
        exit 0
      fi
      sleep 0.2
    done
    echo "the hit list under the search prompt never cleared"
    exit 1
  '

tui_quit
