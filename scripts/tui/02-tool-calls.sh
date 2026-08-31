#!/usr/bin/env bash
# V2 — a tool call renders collapsed, clicking expands and collapses it, and each of the three
# declared render intents draws its own shape. The colour half is asserted at the SCREEN rather
# than in a unit test: the `+` gutter must carry the dark theme's `added` role (#9ece6a, see
# `tui-shell/src/theme.rs`), while the body next to it carries syntect's own colours for `.rs` —
# which is `tui-render/src/diff.rs`'s documented split of gutter role vs. syntax highlight.
# The subject here is the TYPED tool surface: the transcript calls tools by name, and code mode —
# the default consumer since 2026-08-28 — conceals them. `TYPED_TOOLS=1` boots the shipped fallback
# layer (`bundles/bough-typed.yml`).
TYPED_TOOLS=1
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip a_tool_call_renders_collapsed_on_one_line "tool intents are replayed, not live"; exit 0; }

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/tool-calls.patch.yml"

shell-use submit "run the scripted tools"
wait_for "bash"

t a_tool_call_renders_collapsed_on_one_line \
  see "bash" --timeout 20000

shell-use mouse click --on-text "bash"
t clicking_the_header_expands_it \
  see "exit 0" --timeout 5000

shell-use mouse click --on-text "bash"
t clicking_again_collapses_it \
  see "exit 0" --not --timeout 5000

shell-use mouse click --on-text "read_file"
t a_generic_intent_shows_a_key_value_block \
  see "path:" --timeout 5000

shell-use mouse click --on-text "bash"
t a_terminal_intent_shows_monospace_output_and_the_exit_code \
  see "exit 0" --timeout 5000

shell-use mouse click --on-text "write_file"
t a_diff_intent_shows_added_and_removed_lines_in_colour \
  expect_diff_gutter "+fn main() {" "#9ece6a"

tui_quit
