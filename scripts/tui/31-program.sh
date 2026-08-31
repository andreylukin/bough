#!/usr/bin/env bash
# Phase codemode V-TUI — the program row (WP-7).
#
# ONE program is ONE row. Collapsed it is a single line naming the tool, the number of inner calls
# and the wall time; open it shows the JS the model wrote, the console output UNDER the source, and
# the inner calls as nested tool rows carrying the same ✓/✗ marks every tool row carries. Closing it
# restores the one line — including the sub-rows that were open inside it.
#
# The script runs under BOTH consumers, chosen by $BOUGH_CONSUMER (default `typed`):
#   codemode  the `tools-codemode` rows are mounted and the model's one tool is `run`
#   typed     today's tree — the same transcript's calls arrive as PLAIN TOOL ROWS and there is
#             no program row at all, which is the control arm the bench's numbers are read against
#
# Both arms are replay-only: the assertions are about what the surface DRAWS for a given ledger,
# and a live model would choose its own program.
source "$(dirname "$0")/lib.sh"

CONSUMER="${BOUGH_CONSUMER:-typed}"

NAMES=(
  program_row_is_collapsed_by_default
  enter_expands_the_js_block
  console_output_is_under_the_source
  nested_rows_carry_check_marks
  collapse_restores_one_row
  no_program_row_and_plain_tool_rows_instead
)

if [ -n "$BOUGH_LIVE" ]; then
  skip_all "the program row is asserted against a replayed transcript, not a live answer" "${NAMES[@]}"
  exit 0
fi

# ---------------------------------------------------------------------------
# the transcript
# ---------------------------------------------------------------------------
#
# Under `codemode` the model writes ONE program that makes two inner calls and prints. Under
# `typed` the SAME work arrives as the two calls the model would otherwise have had to make
# itself — the control arm: same tools, same ledger content, a different surface.
REPLAY="$HOME_DIR/program.replay.yml"
if [ "$CONSUMER" = "codemode" ]; then
  cat > "$REPLAY" <<'YML'
entries:
  llm.anthropic:
    plugin: llm-replay
    config:
      strict: true
      models: "*"
      rounds:
        - chunks:
            - type: tool_call
              id: call-run-1
              name: run
              input:
                program: |
                  const listing = await bash('echo scripted-program-output', ['fs:list', 'demo:run']);
                  const second = await bash('echo second-call', ['fs:list', 'demo:run']);
                  console.log('program console line');
            - { type: end, stop: tool_use }
        - chunks:
            - { type: text, text: "the program ran" }
            - { type: end, stop: end_turn }
YML
else
  cat > "$REPLAY" <<'YML'
entries:
  llm.anthropic:
    plugin: llm-replay
    config:
      strict: true
      models: "*"
      rounds:
        - chunks:
            - type: tool_call
              id: call-bash-1
              name: bash
              input: { command: "echo scripted-program-output", tags: ["bash", "scripted", "program"] }
            - type: tool_call
              id: call-read-1
              name: read_file
              input: { path: "notes/demo.txt" }
            - { type: end, stop: tool_use }
        - chunks:
            - { type: text, text: "the tools ran" }
            - { type: end, stop: end_turn }
YML
fi

# ---------------------------------------------------------------------------
# the rows
# ---------------------------------------------------------------------------
#
# Code mode is the DEFAULT consumer since 2026-08-28: `js`, `js.quickjs` and `tools.codemode` are
# in `bundles/bough-base.yml`, ENABLED. So neither arm needs a row ADDED any more — both are one
# `--patch` layer over the shipped tree, which is what a patch layer is for.
#
#   codemode  a layer that restates this script's caps and `tags_required: false`
#   typed     `bundles/bough-typed.yml`, the shipped fallback
TYPED_PATCH="$REPO_ROOT/bundles/bough-typed.yml"
CM_PATCH="$HOME_DIR/codemode.patch.yml"
cat > "$CM_PATCH" <<'YML'
entries:
  js:
    config:
      default_caps:
        ops: 20000000
        memory_bytes: 67108864
        stack_bytes: 1048576
        wall_ms: 30000
        console_bytes: 65536
  js.quickjs:
    config:
      interrupt_check_ops: 10000
      max_concurrent_programs: 4
  tools.codemode:
    disabled: false
    config:
      max_console_bytes: 65536
      max_calls_per_program: 64
      tags_required: false
      surface_section: true
YML

tui_open
if [ "$CONSUMER" = "codemode" ]; then
  tui_start "$CM_PATCH" "$REPLAY"
else
  tui_start "$TYPED_PATCH" "$REPLAY"
fi

shell-use submit "run the scripted program"
shell-use wait idle --timeout 30000

if [ "$CONSUMER" != "codemode" ]; then
  # The control arm. The same two calls, as the plain tool rows today's tree draws — and NO
  # program row: there is no `run` tool mounted to make one.
  t no_program_row_and_plain_tool_rows_instead \
    see "bash" --timeout 20000
  # …and the row the codemode arm draws is NOT on screen: no `run`, no program summary.
  t no_program_row_and_plain_tool_rows_instead_draws_no_program_row \
    expect_absent "▸ program"
  skip_all "the program row exists only under \`codemode\`" \
    program_row_is_collapsed_by_default \
    enter_expands_the_js_block \
    console_output_is_under_the_source \
    nested_rows_carry_check_marks \
    collapse_restores_one_row
  tui_quit
  exit 0
fi

# --- the codemode arm -------------------------------------------------------

# Collapsed: ONE line, naming the tool and what it did. The source is NOT on screen yet.
# The gist names its calls when they fit the pane ("the TUI brief" D2; `calls_gist`) and falls
# back to "2 calls" only when they do not, so the row is asserted by its MARKER and a call name,
# never by which side of the width threshold this machine's rail landed on.
t program_row_is_collapsed_by_default \
  row_with "▸ program" "second-call"
t program_row_is_collapsed_by_default_shows_no_source \
  see "await bash" --not --timeout 5000

# The roving row focus + Enter is the keyboard half of the disclosure (`expand.rs`); the mouse
# half is the same toggle, and `02-tool-calls.sh` already pins clicking.
shell-use mouse click --on-text "▸ program"
t enter_expands_the_js_block \
  see "await bash" --timeout 5000

# D-4: the console output IS what the model received, and it sits UNDER the source block.
t console_output_is_under_the_source \
  see "program console line" --timeout 5000
SCREEN="$(shell-use text)"
SRC_LINE="$(echo "$SCREEN" | grep -n "await bash" | head -1 | cut -d: -f1)"
OUT_LINE="$(echo "$SCREEN" | grep -n "program console line" | head -1 | cut -d: -f1)"
t console_output_is_under_the_source_in_that_order \
  test "$SRC_LINE" -lt "$OUT_LINE"

# The inner calls are ordinary tool rows: same header, same ✓.
t nested_rows_carry_check_marks \
  row_with "bash" "✓" 

# Closing it restores the one line — and closes whatever was open inside it.
shell-use mouse click --on-text "▾ program"
t collapse_restores_one_row \
  see "await bash" --not --timeout 5000
t collapse_restores_one_row_keeps_the_header \
  see "program" --timeout 5000

skip_all "the typed control arm runs under BOUGH_CONSUMER=typed" \
  no_program_row_and_plain_tool_rows_instead

tui_quit
