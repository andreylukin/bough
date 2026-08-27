#!/usr/bin/env bash
# SWAP, measured without a model. `25-swap-status.sh` proves the same gate against a REPLAYED turn,
# and reads the reflow off the transcript's row count — which makes its baseline depend on a turn
# completing. This script measures the same reflow off a row that is on screen with no turn at all:
# the SEARCH pane's row index. Disabling `tui.status` frees exactly one row, so every pane above it
# moves down by one; restoring the patch moves it back.
#
# The gate (§17, plan §3 SWAP), in four steps, all while the binary runs and never restarts:
#   1. `tui.status` disabled by patch  -> the line is gone AND the layout reflowed by one row.
#   2. the patch removed               -> the line is back AND the layout reflowed back.
#   3. `tui.search` disabled / restored -> Phase 3's behaviour, unchanged by the new row.
#   4. both disabled at once           -> both gone, the composer still takes keys; both restored.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip the_status_row_reflows_the_layout "the swap gate is composition, not a model"; exit 0; }

# The 1-based screen row a marker is on. The reflow measurement: `search [` is drawn by the search
# pane, which sits ABOVE the status line, so it moves when the status row leaves.
row_of() { shell-use text | grep -n "$1" | head -1 | cut -d: -f1; }
count_of() { shell-use text | grep -c "$1"; }
export -f row_of count_of

tui_open
tui_start
# Wide, so the status row keeps its hints: the drop chain sheds them first on a narrow row and this
# script reads `? help` as the proof that the row is on screen.
shell-use resize 160 40
shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true
sleep 0.6
shell-use keys "Ctrl+f"
sleep 1

pid_before="$(pgrep -f "$BOUGH_BIN" | head -1)"

t the_status_row_and_the_search_row_are_both_on_screen \
  bash -c '[ "$(count_of "? help")" = 1 ] && [ "$(count_of "search \[")" = 1 ]'

search_row_before="$(row_of 'search \[')"
t the_search_row_has_a_measurable_position \
  bash -c '[ -n "'"$search_row_before"'" ]'

# --- 1. `tui.status` disabled while it runs ----------------------------------------------------
write_patch <<'YML'
entries:
  tui.status:
    disabled: true
YML

t disabling_the_status_row_removes_the_line_without_a_restart \
  bash -c '
    for i in $(seq 1 60); do
      [ "$(count_of "? help")" = 0 ] && exit 0
      sleep 0.5
    done
    echo "the status hints are still on screen after the patch"
    exit 1
  '

t the_layout_reflowed_by_exactly_one_row \
  bash -c '
    for i in $(seq 1 60); do
      [ "$(row_of "search \[")" = "$(( '"$search_row_before"' + 1 ))" ] && exit 0
      sleep 0.5
    done
    echo "the search row is at $(row_of "search \["), expected '"$search_row_before"' + 1"
    exit 1
  '

t the_composer_still_takes_keys \
  bash -c '
    shell-use type "still typing"
    see "still typing" --timeout 8000 || { echo "the composer stopped taking keys"; exit 1; }
    shell-use keys "Ctrl+u"
  '

# --- 2. the patch removed ----------------------------------------------------------------------
clear_patch

t removing_the_patch_returns_the_status_row \
  bash -c '
    for i in $(seq 1 60); do
      [ "$(count_of "? help")" = 1 ] && exit 0
      sleep 0.5
    done
    echo "the status line did not come back"
    exit 1
  '

t the_layout_reflowed_back \
  bash -c '
    for i in $(seq 1 60); do
      [ "$(row_of "search \[")" = "'"$search_row_before"'" ] && exit 0
      sleep 0.5
    done
    echo "the search row is at $(row_of "search \["), expected '"$search_row_before"'"
    exit 1
  '

# --- 3. the Phase 3 gate, unchanged: `tui.search` ----------------------------------------------
write_patch <<'YML'
entries:
  tui.search:
    disabled: true
YML

t disabling_the_search_row_removes_the_pane_and_leaves_the_status_row \
  bash -c '
    for i in $(seq 1 60); do
      [ "$(count_of "search \[")" = 0 ] && [ "$(count_of "? help")" = 1 ] && exit 0
      sleep 0.5
    done
    echo "search rows=$(count_of "search \["), status rows=$(count_of "? help")"
    exit 1
  '

clear_patch
t removing_the_patch_returns_the_search_row \
  bash -c '
    for i in $(seq 1 60); do
      shell-use keys "Ctrl+f" >/dev/null 2>&1
      [ "$(count_of "search \[")" = 1 ] && exit 0
      sleep 0.5
    done
    echo "the search pane did not come back"
    exit 1
  '

# --- 4. both at once, then both restored -------------------------------------------------------
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
      if [ "$(count_of "search \[")" = 0 ] && [ "$(count_of "? help")" = 0 ]; then
        shell-use type "both gone"
        see "both gone" --timeout 8000 || exit 1
        shell-use keys "Ctrl+u"
        exit 0
      fi
      sleep 0.5
    done
    echo "the shell did not settle with both rows disabled"
    exit 1
  '

clear_patch
t both_rows_restored \
  bash -c '
    for i in $(seq 1 60); do
      shell-use keys "Ctrl+f" >/dev/null 2>&1
      [ "$(count_of "search \[")" = 1 ] && [ "$(count_of "? help")" = 1 ] && exit 0
      sleep 0.5
    done
    echo "search rows=$(count_of "search \["), status rows=$(count_of "? help")"
    exit 1
  '

t the_process_never_restarted \
  bash -c "[ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

tui_quit
