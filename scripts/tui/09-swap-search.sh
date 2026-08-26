#!/usr/bin/env bash
# SWAP — the search pane row is disabled BY A PATCH FILE while the binary runs. The launcher's
# watch recomposes, the pane leaves, the remaining panes take the freed rows, removing the patch
# brings it back, and the process never restarted.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip the_search_pane_is_on_screen_before_the_patch "the swap gate is composition, not a model"; exit 0; }

USER_PATCH="$HOME_DIR/bough.patch.yml"

# The scroll fixture, so the focus pane is SATURATED with content: "the remaining panes took the
# freed rows" is only observable when there is more to show than fits.
tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/scroll.patch.yml"
shell-use submit "fill the trajectory"
shell-use wait idle --timeout 30000
shell-use keys "Ctrl+f"

t the_search_pane_is_on_screen_before_the_patch \
  see "search" --timeout 20000

pid_of() { shell-use get command >/dev/null 2>&1; pgrep -f "$BOUGH_BIN" | head -1; }
pid_before="$(pid_of)"
# How many TRAJECTORY rows the focus pane is showing. Not `grep -c .` over the whole screen: that
# counts the search pane's own rows too, so removing the pane made the number go DOWN and the
# assertion `>= rows_before` was measuring the opposite of what it claimed.
traj_rows() { shell-use text | grep -c "trajectory line"; }
rows_before="$(traj_rows)"

cat > "$USER_PATCH" <<'YML'
entries:
  tui.search:
    disabled: true
YML

t writing_the_patch_removes_the_pane_without_a_restart \
  see "search" --not --timeout 20000

t the_remaining_panes_resized_to_fill_the_freed_rows \
  bash -c "[ \"\$(shell-use text | grep -c 'trajectory line')\" -gt \"$rows_before\" ]"

rm -f "$USER_PATCH"
t removing_the_patch_returns_the_pane \
  see "search" --timeout 20000

t the_process_never_restarted \
  bash -c "[ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

tui_quit
