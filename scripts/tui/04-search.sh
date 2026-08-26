#!/usr/bin/env bash
# V4 — the FTS pane: focus it, query it, read the hits, click one, and see a bad query reported
# inline instead of thrown.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip ctrl_f_focuses_the_search_pane "the search corpus is the replayed trajectory"; exit 0; }

tui_open
tui_start

shell-use submit "say the whole sentence please"
shell-use wait idle --timeout 30000

shell-use keys "Ctrl+f"
t ctrl_f_focuses_the_search_pane \
  see "search" --timeout 5000

shell-use type "fragment"
t a_query_lists_hits_with_agent_and_step_id \
  see "fragment" --timeout 10000
t a_hit_names_the_agent_that_owns_it \
  see "sol" --timeout 5000

shell-use mouse click --on-text "fragment"
t clicking_a_hit_focuses_that_step_in_the_trajectory \
  see "the first fragment" --timeout 5000

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
