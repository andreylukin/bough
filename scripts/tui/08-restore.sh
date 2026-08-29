#!/usr/bin/env bash
# V8 — the terminal is restored on a boot failure and on a panic, and the failure report is
# READABLE afterwards. This is the script that pins `boot.rs`'s teardown-before-report order: a
# report printed into the alt screen is wiped by the restore, so a passing binary must show it on
# the normal screen.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip a_row_that_never_activates_leaves_the_alt_screen_before_reporting "restoration is model-free"; exit 0; }

NEVER="$(add_row never <<'YML'

- id: tui.never
  plugin: tui-never
  config: {}
YML
)"

PROBE="$(add_row probe <<'YML'

- id: tui.probe
  plugin: tui-probe
  config:
    text: "PROBE-PANE-OK"
    panic_key: "p"
YML
)"

# The restoration assertions, as one function:
#
#   * the NORMAL screen is back — the shell line this session typed before the binary started is
#     readable again, which it would not be from inside the alt screen;
#   * typed characters echo, which is what "raw mode is off" means from outside the process.
#
# Cursor VISIBILITY is deliberately not asserted here: `shell-use get cursor` reports the cursor's
# position and nothing else, so there is no way to read the DECTCEM state from outside. The
# `Show` half of the restore is covered by `tui-shell`'s `term::restores` observer instead
# (`plugins/tui-shell/tests/restore.rs`).
assert_restored() {
  see "export BOUGH_HOME" --timeout 20000 >/dev/null \
    || { echo "the normal screen is not back: the alt screen was never left"; return 1; }
  shell-use type "echo RESTORED-ECHO"
  see "RESTORED-ECHO" --timeout 5000 >/dev/null || { echo "typed characters do not echo: raw mode is still on"; return 1; }
  shell-use press Enter
  sleep 0.5
}

tui_open
tui_start "$NEVER"
wait_for "never activated" 30000

t a_row_that_never_activates_leaves_the_alt_screen_before_reporting \
  bash -c 'see "never activated" --timeout 20000 && see "tui.never" --timeout 5000'
t a_row_that_never_activates_leaves_the_alt_screen_before_reporting.restored assert_restored

tui_close

EXIT_FILE="$HOME_DIR/probe.exit"
tui_open
tui_start_recording_exit "$EXIT_FILE" "$PROBE"
see "PROBE-PANE-OK" --timeout 20000 >/dev/null \
  || { echo "not ok - the probe pane never rendered"; tui_close; exit 1; }
# Give the PANE the keyboard first. Since phase ux1 a click never steals typing (B1) — the
# composer stays live — so the keyboard is moved the way a keyboard user moves it: Tab. The ring
# order is not this script's business, so it walks it: i Tabs from the composer, then `p`. A `p`
# that reaches the composer instead is typed into the draft, which returns the keyboard to the
# composer, so the next iteration starts from the same place and walks one stop further.
arm_the_probe() {
  local i j
  # `Ctrl+F` is the one keystroke that names a pane, so it is the fixed point the walk starts
  # from: focus the search pane, then take `i` Tabs around the ring and offer `p`. A `p` that
  # reaches the composer is typed into the draft and moves the keyboard back — which is why the
  # next iteration re-anchors with `Ctrl+F` rather than assuming where it ended up.
  for i in $(seq 1 6); do
    shell-use press "Ctrl+f" >/dev/null 2>&1 || true
    for j in $(seq 1 "$i"); do shell-use press Tab >/dev/null; done
    shell-use press p >/dev/null
    sleep 0.5
    if see "panic" --timeout 1000 >/dev/null 2>&1 || [ -s "$EXIT_FILE" ]; then return 0; fi
    shell-use press "Ctrl+u" >/dev/null 2>&1 || true
  done
  return 0
}
arm_the_probe

t a_panic_inside_a_pane_restores_the_terminal_and_exits_non_zero \
  bash -c 'see "panic" --timeout 20000'
# 101 exactly: that is the code `tui-shell`'s loop asks the kernel for when a pane's render
# unwinds (`lib.rs`, V8), so a plain "non-zero" would also pass on a boot failure.
t a_panic_inside_a_pane_restores_the_terminal_and_exits_non_zero.code \
  await_exit_code "$EXIT_FILE" 101
t a_panic_inside_a_pane_restores_the_terminal_and_exits_non_zero.restored assert_restored
