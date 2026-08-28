#!/usr/bin/env bash
# SWAP — phase c's swap gate (§17 Phase 8, plan §5 SWAP). The rows this phase ADDED are the ones it
# disables: `tui.preview`, `tui.timeline`, `tui.drift`, one at a time, by a patch file written
# WHILE the binary runs. Each has to leave, the layout has to reflow, and putting all three back
# has to restore them in the same process.
#
# The geometry the reflow bullets read (`tui-shell::pane::layout`): the `Aux` band is anchored at
# the BOTTOM of what is left above the status line, and its panes stack in `order`. A pane leaving
# therefore shortens the band and its TOP EDGE DROPS by that pane's rows, while the focus pane
# above it grows by the same amount. The panes BELOW the one that left do not move at all — they
# are pinned to the band's bottom — which is why every reflow bullet here measures the band's top
# edge (the first digging header on screen) and not "some pane moved". A bullet written the other
# way passed by accident on the pane that happened to be above and asserted nothing on the rest.
#
# The three panes are catalog rows in no bundle (D-C10), so this script mounts all three itself.
# It is the only script that mounts more than one: the swap gate is about the band being shared.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && {
  skip_all "the swap gate is composition, not a model" \
  all_three_digging_panes_are_on_screen_before_the_patch \
  disabling_the_preview_row_removes_it_and_the_layout_reflows \
  disabling_the_timeline_row_removes_it_and_the_layout_reflows \
  disabling_the_drift_row_removes_it_and_the_layout_reflows \
  re_enabling_all_three_restores_them_without_a_restart
  exit 0
}

ROW="$(add_row digging <<'YML'

- id: tui.preview
  plugin: tui-preview
  config:
    height: 5
    collapse_rows: 10
    min_rows: 3
    max_rows: 10
    refresh_ms: 250
    max_chars: 200000

- id: tui.timeline
  plugin: tui-timeline
  config:
    height: 5
    collapse_rows: 10
    min_rows: 3
    max_rows: 10
    window: 400
    limit: 200
    debounce_ms: 150
    time_format: "%H:%M:%S"

- id: tui.drift
  plugin: tui-drift
  config:
    height: 5
    collapse_rows: 10
    min_rows: 3
    max_rows: 10
    agents_shown: 12
    refresh_ms: 1000
    bar_cols: 12
    arm_ms: 3000
YML
)"

tui_open
tui_start "$ROW"

# The header each pane always paints, whether or not it has been focused or has data yet
# (`preview · nothing taken yet`, the timeline's `filter [`, `drift · N agents`). A marker that
# only appears after a command would make "the row is gone" indistinguishable from "the command
# was never typed".
PREVIEW_MARK="preview ·"
TIMELINE_MARK="filter ["
DRIFT_MARK="drift ·"
export PREVIEW_MARK TIMELINE_MARK DRIFT_MARK

# `marker_row <needle>`: the 1-based screen row the marker is on, or 0 when it is off screen.
marker_row() {
  shell-use text | grep -nF -- "$1" | head -1 | cut -d: -f1 | tr -d '[:space:]'
}
export -f marker_row

# `gone <needle>`: the marker leaves the screen within the reload window.
gone() {
  local i
  for i in $(seq 1 40); do
    shell-use text | grep -qF -- "$1" || return 0
    sleep 0.5
  done
  echo "still on screen after the patch: $1"
  return 1
}
export -f gone

# `band_top`: the screen row the `Aux` band starts on — the topmost digging header on screen, or 0
# when no digging pane is up.
band_top() {
  local n best=0 mark
  for mark in "$PREVIEW_MARK" "$TIMELINE_MARK" "$DRIFT_MARK"; do
    n="$(marker_row "$mark")"
    [ -n "$n" ] || continue
    if [ "$best" -eq 0 ] || [ "$n" -lt "$best" ]; then best="$n"; fi
  done
  echo "$best"
}
export -f band_top

# `band_dropped <was>`: the band's top edge is LOWER on the screen than it was, which is the same
# statement as "the focus pane above it grew". The direction `pane::layout` says a shorter band
# must move in.
band_dropped() {
  local was="$1" now i
  for i in $(seq 1 40); do
    now="$(band_top)"
    [ "${now:-0}" -gt "${was:-0}" ] && return 0
    sleep 0.5
  done
  echo "the digging band started on row ${was:-0} and starts on row $(band_top): it did not reflow"
  return 1
}
export -f band_dropped

pid_before="$(pgrep -f "$BOUGH_BIN" | head -1)"

t all_three_digging_panes_are_on_screen_before_the_patch \
  bash -c '
    for i in $(seq 1 40); do
      txt="$(shell-use text)"
      printf "%s" "$txt" | grep -qF -- "$PREVIEW_MARK"  || { sleep 0.5; continue; }
      printf "%s" "$txt" | grep -qF -- "$TIMELINE_MARK" || { sleep 0.5; continue; }
      printf "%s" "$txt" | grep -qF -- "$DRIFT_MARK"    || { sleep 0.5; continue; }
      exit 0
    done
    echo "the three digging panes did not all reach the screen"
    exit 1
  '

# --- 1. `tui.preview` out: it goes, and the two below it drop a row. --------------------------
band_before="$(band_top)"
write_patch <<'YML'
entries:
  tui.preview:
    disabled: true
YML

t disabling_the_preview_row_removes_it_and_the_layout_reflows \
  bash -c 'gone "$PREVIEW_MARK" && band_dropped '"${band_before:-0}"''

clear_patch
see "$PREVIEW_MARK" --timeout 20000 >/dev/null 2>&1 || true

# --- 2. `tui.timeline` out: the drift board below it drops a row. -----------------------------
band_before="$(band_top)"
write_patch <<'YML'
entries:
  tui.timeline:
    disabled: true
YML

t disabling_the_timeline_row_removes_it_and_the_layout_reflows \
  bash -c 'gone "$TIMELINE_MARK" && band_dropped '"${band_before:-0}"''

clear_patch
see "$TIMELINE_MARK" --timeout 20000 >/dev/null 2>&1 || true

# --- 3. `tui.drift` out: the band's bottom pane leaves and the two above it drop. -------------
band_before="$(band_top)"
write_patch <<'YML'
entries:
  tui.drift:
    disabled: true
YML

t disabling_the_drift_row_removes_it_and_the_layout_reflows \
  bash -c 'gone "$DRIFT_MARK" && band_dropped '"${band_before:-0}"''

# --- 4. All three back, in the same process. ---------------------------------------------------
clear_patch

t re_enabling_all_three_restores_them_without_a_restart \
  bash -c '
    for i in $(seq 1 60); do
      txt="$(shell-use text)"
      if printf "%s" "$txt" | grep -qF -- "$PREVIEW_MARK" \
      && printf "%s" "$txt" | grep -qF -- "$TIMELINE_MARK" \
      && printf "%s" "$txt" | grep -qF -- "$DRIFT_MARK"; then
        [ "$(pgrep -f "$BOUGH_BIN" | head -1)" = "'"$pid_before"'" ] || {
          echo "the three panes came back but the process restarted"; exit 1; }
        exit 0
      fi
      sleep 0.5
    done
    echo "the three digging panes did not come back"
    exit 1
  '

tui_quit
