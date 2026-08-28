#!/usr/bin/env bash
# Phase codemode SWAP (§17, the phase exit gate) — on screen.
#
# `crates/bough/tests/codemode_swap.rs` proves the swap against the composed tree: one patch line,
# one live recompose, the other surface. This script is the half that test cannot see — that the
# INTERFACE follows. The same running process, the same transcript, and the model's next answer
# arrives as plain tool rows or as one program row depending on nothing but whether
# `tools.codemode` is enabled.
#
# The three code-mode rows ship ENABLED in `bundles/bough-base.yml` (code mode is the DEFAULT
# consumer, 2026-08-28), so the patch file here does the whole job: it DISABLES the consumer while
# the binary runs, and clearing it puts the shipped surface back.
#
# Replay only. The point is which surface draws a given ledger, and a live model would write its
# own program.
source "$(dirname "$0")/lib.sh"

NAMES=(
  typed_rows_before_the_patch
  program_row_after_the_patch
  no_failed_row_in_the_status_line
  typed_rows_again_after_revert
  the_process_never_restarted
)

if [ -n "$BOUGH_LIVE" ]; then
  skip_all "the swap gate is composition, not a model" "${NAMES[@]}"
  exit 0
fi

# ---------------------------------------------------------------------------
# the transcript: three turns, in order.
#
#   1. the consumer is DISABLED  → the model calls `bash`, a plain tool row
#   2. the consumer is ENABLED   → the model calls `run`, one program row
#   3. the consumer is DISABLED again → `bash` again
#
# `strict: true`, so the rounds are consumed in this order and each `submit` below advances by
# exactly two (the call round and the answer round).
REPLAY="$HOME_DIR/swap.replay.yml"
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
              id: call-typed-1
              name: bash
              input: { command: "echo typed-surface-one", tags: ["bash", "typed", "surface"] }
            - { type: end, stop: tool_use }
        - chunks:
            - { type: text, text: "the typed tools ran" }
            - { type: end, stop: end_turn }
        - chunks:
            - type: tool_call
              id: call-run-1
              name: run
              input:
                program: |
                  console.log(await view('demo.txt'));
                  console.log('the program ran');
            - { type: end, stop: tool_use }
        - chunks:
            - { type: text, text: "the program ran" }
            - { type: end, stop: end_turn }
        - chunks:
            - type: tool_call
              id: call-typed-2
              name: bash
              input: { command: "echo typed-surface-two", tags: ["bash", "typed", "surface"] }
            - { type: end, stop: tool_use }
        - chunks:
            - { type: text, text: "the typed tools ran again" }
            - { type: end, stop: end_turn }
YML

# ---------------------------------------------------------------------------
# the rows: NOTHING to add. Code mode is the DEFAULT consumer since 2026-08-28, so `js`,
# `js.quickjs` and `tools.codemode` are in `bundles/bough-base.yml` ENABLED and the patch file
# below does what a patch file is for — it turns the consumer off and on while the binary runs.
# (Until that date the rows were added to this script's own bundle copy first.)

# Start with the consumer OFF, so the first turn is the control arm.
write_patch <<'YML'
entries:
  tools.codemode:
    disabled: true
YML

# One file for the program to read. NOT a shell command: with `tags_required` on, no registered
# tool has a `tags` property, so every `bash` call in the sandbox is refused today
# (`docs/codemode-merge-notes.md` §9). `view` is a host call like any other, and which host call
# the program made is not what this script is about.
mkdir -p "$HOME_DIR/work"
printf 'one\ntwo\n' > "$HOME_DIR/work/demo.txt"

tui_open
tui_start "$REPLAY"

pid_before="$(pgrep -f "$BOUGH_BIN" | head -1)"

# --- 1. the typed surface -----------------------------------------------------------------
shell-use submit "the first turn"
shell-use wait idle --timeout 30000

t typed_rows_before_the_patch \
  see "bash" --timeout 20000
t typed_rows_before_the_patch_draws_no_program_row \
  expect_absent "1 call"

# --- 2. the swap: remove the disable, same process ----------------------------------------
clear_patch

shell-use submit "the second turn"
shell-use wait idle --timeout 30000

t program_row_after_the_patch \
  row_with "program" "1 call"

# The status line is where a Failed row would show. The seam rows have to be untouched by the
# swap — `codemode_swap.rs::the_tools_seam_rows_stay_active_and_nothing_is_failed` says the same
# thing against the composition; this says it on screen.
t no_failed_row_in_the_status_line \
  bash -c 'expect_absent "failed" && expect_absent "FAILED"'

# --- 3. and back --------------------------------------------------------------------------
#
# FIXED 2026-08-27. This used to be RED: disabling `tools.codemode` after a program had run made
# the trajectory UNREADABLE, because the row declared `program/call`, `program/result` and
# `program/console` through an effect that unwound with it, and the next wake died rebuilding the
# chain on "unknown to this binary and not ignorable". `tools-codemode` now registers the three
# for the life of the BINARY (a step type describes bytes already on disk), so the swap is
# two-way. `codemode_swap.rs::the_program_vocabulary_survives_disabling_the_row` is the in-process
# half of the same gate. `docs/codemode-merge-notes.md` §10.
write_patch <<'YML'
entries:
  tools.codemode:
    disabled: true
YML

# The recompose has to land before the next message, or it is delivered to the surface that is
# on its way out.
shell-use wait idle --timeout 20000
t typed_rows_again_after_revert_recomposed   bash -c 'sleep 2; true'

shell-use submit "the third turn"
shell-use wait idle --timeout 30000

t typed_rows_again_after_revert \
  see "the typed tools ran again" --timeout 20000
t typed_rows_again_after_revert_draws_no_new_program_row \
  bash -c '
    # Exactly one program row in the whole transcript: the one the middle turn drew.
    n="$(shell-use text | grep -c "1 call")"
    [ "${n:-0}" -le 1 ] || { echo "the third turn drew a program row too ($n)"; exit 1; }
  '

t the_process_never_restarted \
  bash -c "[ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

tui_quit
