#!/usr/bin/env bash
# §11 "The resident" — the attach transport with the REAL client in REAL PTYs. The resident boots
# headless in the background; a bare `bough` attaches and drives the same composition; a second
# bare `bough` in ANOTHER terminal takes over (the first is told why, and the un-sent draft is
# still there — one process, one state); `/detach` gives a terminal back while bough keeps
# running; the two-press exit from an attached client takes the resident down, the farewell
# reaches the client, and the socket file is gone. The wire itself is proven by
# `crates/bough/tests/attach.rs`; what only this script can prove is the CLIENT half — raw mode
# on a real tty, the painted screen, the restore on every way out.
source "$(dirname "$0")/lib.sh"

BULLETS="the_resident_boots_headless_and_binds_the_socket \
  a_bare_bough_attaches_and_shows_the_running_tree \
  typing_lands_in_the_resident_not_the_terminal \
  a_second_attach_takes_over_and_the_first_is_told_why \
  the_conversation_survives_the_handover \
  slash_detach_returns_the_terminal_and_bough_survives \
  ctrl_c_twice_from_a_fresh_client_takes_the_resident_down"

# shellcheck disable=SC2086
[ -n "$BOUGH_LIVE" ] && { skip_all "the transport has no model in it" $BULLETS; exit 0; }

SOCKET="$HOME_DIR/tui.sock"
MARK="a draft that lives in the resident"

# The SECOND terminal. `TUI_TEST_SESSION` is the env var shell-use actually reads, so prefixing a
# call with `in2` points it — the lib's helpers included — at this PTY instead of the first.
SESSION2="bough-tui-39-b-$$"
in2() { TUI_TEST_SESSION="$SESSION2" SHELL_USE_SESSION="$SESSION2" "$@"; }
close2() { in2 shell-use close >/dev/null 2>&1 || true; }
trap 'close2; tui_close' EXIT

tui_open

# The resident: stdout redirected, so `backend: auto` resolves headless; a background job of the
# PTY's shell, so it shares BOUGH_HOME and dies with the session if a bullet aborts early.
shell-use submit "$BOUGH_BIN --resident $(bough_patch_args) >/dev/null 2>&1 &" >/dev/null
t the_resident_boots_headless_and_binds_the_socket \
  bash -c "for i in \$(seq 1 120); do [ -S \"$SOCKET\" ] && exit 0; sleep 0.25; done; exit 1"

# A bare `bough` — no flags, no patches — is the attach client (§0.1 item 2). The screen it paints
# is the RESIDENT's composition: the composer's placeholder, then the rail's first agent.
shell-use submit "$BOUGH_BIN" >/dev/null
wait_for "$BOOT_MARK" 20000
wait_for "sol" 15000
t a_bare_bough_attaches_and_shows_the_running_tree \
  see "sol" --timeout 5000

shell-use type "$MARK"
t typing_lands_in_the_resident_not_the_terminal \
  see "$MARK" --timeout 10000

# The second terminal attaches. ONE client at a time: the first gets its shell back with the
# reason on the normal screen, and the second paints the same composition — draft included.
in2 shell-use open --cols 120 --rows 40 --cwd "$HOME_DIR/work" >/dev/null
in2 shell-use submit "export BOUGH_HOME=$HOME_DIR" >/dev/null
in2 wait_for "export BOUGH_HOME" 5000
in2 shell-use submit "$BOUGH_BIN" >/dev/null
t a_second_attach_takes_over_and_the_first_is_told_why \
  see "another bough attached" --timeout 15000
t the_conversation_survives_the_handover \
  in2 see "$MARK" --timeout 15000

# `/detach` (a command of the `tui.attach` row): this terminal comes back, bough keeps running.
in2 shell-use press "Ctrl+u"
in2 shell-use submit "/detach" >/dev/null
in2 wait_for "bough: detached" 15000
t slash_detach_returns_the_terminal_and_bough_survives \
  bash -c "[ -S \"$SOCKET\" ] && pgrep -f -- \"$BOUGH_BIN --resident\" >/dev/null"

# A fresh client, then the two-press exit (B7): the resident tears down, the EXIT frame reaches
# the client as its farewell line, and the socket goes with the process.
in2 shell-use submit "$BOUGH_BIN" >/dev/null
in2 wait_for "$BOOT_MARK" 20000
in2 shell-use press "Ctrl+c"
in2 wait_for "again to exit" 10000
in2 shell-use press "Ctrl+c"
in2 wait_for "bough: bough exited" 15000
t ctrl_c_twice_from_a_fresh_client_takes_the_resident_down \
  bash -c "for i in \$(seq 1 60); do \
             ! pgrep -f -- \"$BOUGH_BIN --resident\" >/dev/null && [ ! -S \"$SOCKET\" ] && exit 0; \
             sleep 0.25; done; exit 1"

close2
tui_close
exit 0
