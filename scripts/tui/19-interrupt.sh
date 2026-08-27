#!/usr/bin/env bash
# V4 — interrupt and exit (phase ux1 §2.4). Blockers 7 and 8: a running turn could not be stopped
# and the personas never learned it could, and `/quit` left a blank screen or hung. What this
# script pins is that a stop is offered, works, and is VISIBLE, and that both exit paths end with
# the terminal restored and a shell prompt back.
#
# The slow transcript, because every bullet but the last two is about a turn that is still running.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip esc_interrupts_and_marks_it "interrupt timing needs the slow replay transcript"; exit 0; }

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/slow.patch.yml"

shell-use submit "start something long"
t the_answer_is_running \
  see "starting a long answer" --timeout 20000

# --- The stop key is NAMED while the turn runs. -----------------------------------------------
#
# The audit's finding is not that Esc did nothing; it is that no persona knew to try it. So the
# assertion is on the chrome, not on the binding.
t the_stop_key_is_named_while_running \
  see "esc" --timeout 10000

# --- Esc stops it, and the transcript SAYS it was stopped. ------------------------------------
shell-use press Escape
t esc_interrupts_and_marks_it \
  see "interrupted" --timeout 20000

t the_interrupted_turn_really_stopped \
  bash -c '
    # No further chunk of the slow round lands after the marker. The fixture would have added
    # " and going" twice more over the next eight seconds.
    sleep 9
    shell-use text | grep -q "and going and going" && { echo "the answer kept streaming after the interrupt"; exit 1; }
    exit 0
  '

t the_composer_still_takes_a_message_after_an_interrupt \
  bash -c 'shell-use type "still here" && see "still here" --timeout 8000'
shell-use keys "Ctrl+u"

# --- Idle Ctrl+C asks first, and the second one exits. ----------------------------------------
shell-use keys "Ctrl+c"
t an_idle_ctrl_c_asks_before_exiting \
  see "again to exit" --timeout 10000

t the_second_ctrl_c_exits_with_the_terminal_restored \
  bash -c '
    shell-use keys "Ctrl+c"
    shell-use wait idle --timeout 15000 >/dev/null 2>&1 || true
    # "Raw mode is off" means the shell echoes typed characters again — the same assertion
    # `08-restore.sh` makes, and the only one that distinguishes a restored terminal from a
    # process that merely died.
    shell-use type "echo terminal-is-back"
    see "echo terminal-is-back" --timeout 8000 || { echo "the shell does not echo: raw mode was never left"; exit 1; }
    shell-use press Enter
    see "terminal-is-back" --timeout 8000
  '

# --- `/quit` says goodbye, and is gone inside the bounded teardown window. ---------------------
EXITFILE="$HOME_DIR/quit.exit"
tui_start_recording_exit "$EXITFILE" "$REPO_ROOT/scripts/tui/fixtures/slow.patch.yml"
shell-use wait idle --timeout 20000 >/dev/null 2>&1 || true

started="$(date +%s)"
shell-use submit "/quit"
t quit_says_goodbye_and_is_gone_within_two_seconds \
  bash -c '
    started='"$started"'
    await_exit_code "'"$EXITFILE"'" 0 || exit 1
    took=$(( $(date +%s) - started ))
    # `Cli::shutdown_ms` is 2000; the bullet allows the whole teardown plus the PTY round trip.
    [ "$took" -le 6 ] || { echo "/quit took ${took}s"; exit 1; }
    exit 0
  '

t the_farewell_is_one_line_and_the_screen_is_not_blank \
  bash -c '
    txt="$(shell-use text | sed "s/[[:space:]]*$//" | grep -v "^$" | tail -20)"
    [ -n "$txt" ] || { echo "the screen is blank after /quit"; exit 1; }
    printf "%s\n" "$txt" | grep -qi "bough\|bye\|goodbye" || {
      echo "no farewell line on screen after /quit:"; printf "%s\n" "$txt"; exit 1; }
  '

tui_close
