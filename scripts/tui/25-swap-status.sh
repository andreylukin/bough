#!/usr/bin/env bash
# SWAP — phase ux1's swap gate (§17, plan §3 SWAP). The row this phase ADDED is the one it disables:
# `tui.status`, by a patch file written WHILE the binary runs. The line has to leave, the transcript
# has to take the freed row, removing the patch has to bring it back, and the process must never
# have restarted.
#
# Then the same three steps for `tui.search` — Phase 3's `09-swap-search.sh` behaviour, unchanged,
# re-run here to prove the new row did not break the old gate — and then both at once.
#
# A swap test that cannot fail proves nothing: this is why the status line is a ROW of its own and
# not a field of `tui.strip` (plan §1).
source "$(dirname "$0")/lib.sh"

# The live half does not run this script. Every bullet it carries is named here, so the
# skip COUNT matches the count the replay half prints (a whole-script skip printing one
# `ok` line for ten assertions is the dishonesty `skip` exists to avoid).
[ -n "$BOUGH_LIVE" ] && {
  skip_all "the swap gate is composition, not a model" \
  the_scroll_fixture_filled_the_transcript \
  the_status_row_is_on_screen_before_the_patch \
  disabling_the_status_row_removes_the_line_without_a_restart \
  the_transcript_grew_by_exactly_one_row \
  nothing_else_moved \
  removing_the_patch_returns_the_status_row \
  the_layout_reflowed_back \
  the_search_row_is_on_screen_before_its_patch \
  disabling_the_search_row_removes_the_pane \
  ctrl_f_degrades_to_a_notice_with_the_row_disabled \
  removing_the_patch_returns_the_search_row \
  both_rows_disabled_at_once_leaves_a_working_shell \
  both_rows_restored \
  the_process_never_restarted
  exit 0
}

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/scroll.patch.yml"
shell-use submit "fill the trajectory"
shell-use wait idle --timeout 30000
# `wait idle` returns when the PTY goes quiet, which on a loaded machine can happen BEFORE the
# replayed turn has painted anything. Every row count below is a delta against this screen, so the
# baseline has to be taken after the fixture's content is actually on it — a baseline of 0 turns
# "grew by exactly one row" into an assertion about nothing.
t the_scroll_fixture_filled_the_transcript \
  see "trajectory line" --timeout 60000

# How many TRAJECTORY rows the transcript is showing. The same measurement `09-swap-search.sh`
# uses, and for the same reason: a count over the whole screen counts the chrome that is leaving.
traj_rows() { shell-use text | grep -c "trajectory line"; }
export -f traj_rows

# Wide, so the hints have room: the drop chain sheds them first on a narrow row, and this script
# reads the hints as the proof that the status ROW is on screen.
shell-use resize 160 40
shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true
sleep 0.6

pid_of() { pgrep -f "$BOUGH_BIN" | head -1; }
pid_before="$(pid_of)"

t the_status_row_is_on_screen_before_the_patch \
  bash -c 'see "? help" --timeout 20000 && see "bough" --timeout 8000'

rows_before="$(traj_rows)"

# --- 1. Disable `tui.status` while it runs. ----------------------------------------------------
write_patch <<'YML'
entries:
  tui.status:
    disabled: true
YML

t disabling_the_status_row_removes_the_line_without_a_restart \
  bash -c '
    for i in $(seq 1 60); do
      shell-use text | grep -q "? help" || exit 0
      sleep 0.5
    done
    echo "the status hints are still on screen after the patch"
    exit 1
  '

t the_transcript_grew_by_exactly_one_row \
  bash -c '
    for i in $(seq 1 60); do
      now="$(traj_rows)"
      [ "${now:-0}" -eq $(( '"${rows_before:-0}"' + 1 )) ] && exit 0
      sleep 0.5
    done
    echo "the transcript is $(traj_rows) rows, expected '"${rows_before:-0}"' + 1"
    exit 1
  '

t nothing_else_moved \
  bash -c '
    # The rail is still drawn and the composer is still live: the freed row went to the transcript
    # and the rest of the layout is where it was.
    shell-use type "still typing"
    see "still typing" --timeout 8000 || { echo "the composer stopped taking keys"; exit 1; }
    shell-use keys "Ctrl+u"
  '

# --- 2. Remove the patch: the row comes back and the layout reflows back. ---------------------
clear_patch

t removing_the_patch_returns_the_status_row \
  bash -c 'see "? help" --timeout 20000'

t the_layout_reflowed_back \
  bash -c '
    for i in $(seq 1 60); do
      [ "$(traj_rows)" = "'"${rows_before:-0}"'" ] && exit 0
      sleep 0.5
    done
    echo "the transcript is $(traj_rows) rows, expected '"${rows_before:-0}"'"
    exit 1
  '

# --- 3. The Phase 3 gate, unchanged: `tui.search`. --------------------------------------------
shell-use keys "Ctrl+f"
t the_search_row_is_on_screen_before_its_patch \
  see "search" --timeout 20000
shell-use press Escape

write_patch <<'YML'
entries:
  tui.search:
    disabled: true
YML

t disabling_the_search_row_removes_the_pane \
  bash -c 'see "search" --not --timeout 20000'

t ctrl_f_degrades_to_a_notice_with_the_row_disabled \
  bash -c '
    shell-use keys "Ctrl+f"
    for i in $(seq 1 20); do
      shell-use text | grep -qi "no search\|search.*not\|unavailable" && exit 0
      sleep 0.5
    done
    echo "Ctrl+F with the search row disabled said nothing"
    exit 1
  '

clear_patch
t removing_the_patch_returns_the_search_row \
  bash -c 'shell-use keys "Ctrl+f"; see "search" --timeout 20000'
shell-use press Escape

# --- 4. Both rows disabled at once, then both restored. ---------------------------------------
write_patch <<'YML'
entries:
  tui.status:
    disabled: true
  tui.search:
    disabled: true
YML

t both_rows_disabled_at_once_leaves_a_working_shell \
  bash -c '
    for i in $(seq 1 60); do
      txt="$(shell-use text)"
      printf "%s" "$txt" | grep -q "? help" && { sleep 0.5; continue; }
      printf "%s" "$txt" | grep -q "trajectory line" || { sleep 0.5; continue; }
      shell-use type "both gone"
      see "both gone" --timeout 8000 || exit 1
      shell-use keys "Ctrl+u"
      exit 0
    done
    echo "the shell did not settle with both rows disabled"
    exit 1
  '

clear_patch
t both_rows_restored \
  bash -c '
    see "? help" --timeout 20000 || exit 1
    shell-use keys "Ctrl+f"
    see "search" --timeout 20000 || exit 1
    shell-use press Escape
  '

t the_process_never_restarted \
  bash -c "[ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

tui_quit
