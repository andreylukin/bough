#!/usr/bin/env bash
# V4 — interrupt and exit (phase ux1 §2.4). Blockers 7 and 8: a running turn could not be stopped
# and the personas never learned it could, and `/quit` left a blank screen or hung. What this
# script pins is that a stop is offered, works, and is VISIBLE, and that both exit paths end with
# the terminal restored and a shell prompt back.
#
# The slow transcript, because every bullet but the last two is about a turn that is still running.
source "$(dirname "$0")/lib.sh"

# The live half does not run this script. Every bullet it carries is named here, so the
# skip COUNT matches the count the replay half prints (a whole-script skip printing one
# `ok` line for ten assertions is the dishonesty `skip` exists to avoid).
[ -n "$BOUGH_LIVE" ] && {
  skip_all "interrupt timing needs the slow replay transcript" \
  the_stop_key_is_absent_while_idle \
  the_answer_is_running \
  the_stop_key_is_named_while_running \
  esc_interrupts_and_marks_it \
  the_stop_key_is_gone_once_nothing_runs \
  the_interrupted_turn_really_stopped \
  the_composer_still_takes_a_message_after_an_interrupt \
  an_idle_ctrl_c_asks_before_exiting \
  the_second_ctrl_c_exits_with_the_terminal_restored \
  quit_exits_cleanly_within_three_seconds \
  the_farewell_is_one_line_and_the_screen_is_not_blank
  exit 0
}

tui_open
tui_start "$REPO_ROOT/scripts/tui/fixtures/slow.patch.yml"

# The IDLE baseline for M14: with nothing running, the status line does not name a stop key. This
# is the half that makes `the_stop_key_is_named_while_running` able to fail.
t the_stop_key_is_absent_while_idle \
  bash -c '! shell-use text | grep -qF "esc to interrupt"'

shell-use submit "start something long"
t the_answer_is_running \
  see "starting a long answer" --timeout 20000

# --- The stop key is NAMED while the turn runs. -----------------------------------------------
#
# The audit's finding is not that Esc did nothing; it is that no persona knew to try it. So the
# assertion is on the chrome, not on the binding.
#
# `see "esc"` was VACUOUS: `esc = interrupt` used to be a STATIC entry in `tui.status.hints`,
# rendered at every width that fits, idle or running — so the bullet passed on a build that never
# varied the hint. It is `Field::StopKey` now, which exists ONLY while a turn is running, and the
# phrase is `status::STOP_KEY`. The idle baseline below is what makes this able to fail.
t the_stop_key_is_named_while_running \
  see "esc to interrupt" --timeout 10000

# --- Esc stops it, and the transcript SAYS it was stopped. ------------------------------------
shell-use press Escape
# The DURABLE marker, not the transient shell notice. `run::interrupt` cancels with
# `CancelCause::User`, which the loop maps to `WakeEndReason::Aborted` (§5 reserves `interrupted`
# for a PREEMPTED wake), so the transcript rendered `turn ended · aborted` while this bullet was
# satisfied by the notice band saying "interrupted". `turn interrupted` is the row's own words.
t esc_interrupts_and_marks_it \
  see "turn interrupted" --timeout 20000

# …and the stop key is gone again once nothing is running (the other half of M14).
t the_stop_key_is_gone_once_nothing_runs \
  bash -c '
    for i in $(seq 1 25); do
      shell-use text | grep -qF "esc to interrupt" || exit 0
      sleep 0.4
    done
    echo "the status line still names the stop key with no turn running"
    exit 1
  '

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
shell-use press "Ctrl+u"

# --- Idle Ctrl+C asks first, and the second one exits. ----------------------------------------
shell-use press "Ctrl+c"
t an_idle_ctrl_c_asks_before_exiting \
  see "again to exit" --timeout 10000

t the_second_ctrl_c_exits_with_the_terminal_restored \
  bash -c '
    shell-use press "Ctrl+c"
    wait_for "export BOUGH_HOME" 15000
    # "Raw mode is off" means the shell echoes typed characters again — the same assertion
    # `08-restore.sh` makes, and the only one that distinguishes a restored terminal from a
    # process that merely died.
    shell-use type "echo terminal-is-back"
    see "echo terminal-is-back" --timeout 8000 || { echo "the shell does not echo: raw mode was never left"; exit 1; }
    shell-use press Enter
    see "terminal-is-back" --timeout 8000
  '

# --- `/quit` says goodbye, and is gone inside the bounded teardown window. ---------------------
#
# Clear the PTY's primary buffer first. The Ctrl+C exit above already printed one farewell, and
# `the_farewell_is_one_line_and_the_screen_is_not_blank` reads the SCREEN, not this session's
# output — without this, that bullet counts two farewells from two real exits and fails on a
# scrollback artifact rather than on a banner.
shell-use type "clear"
shell-use press Enter
shell-use wait idle --timeout 8000 >/dev/null 2>&1 || true

EXITFILE="$HOME_DIR/quit.exit"
tui_start_recording_exit "$EXITFILE" "$REPO_ROOT/scripts/tui/fixtures/slow.patch.yml"
sleep 1

started="$(date +%s)"
shell-use submit "/quit"
t quit_exits_cleanly_within_three_seconds \
  bash -c '
    started='"$started"'
    await_exit_code "'"$EXITFILE"'" 0 || exit 1
    took=$(( $(date +%s) - started ))
    # `Cli::shutdown_ms` is 2000. The window is the bounded teardown plus the PTY round trip and
    # the shell redrawing its prompt; 3s is the tightest bound that is not flaky here, and the
    # bullet is named for what it asserts rather than for the deadline it does not measure.
    [ "$took" -le 3 ] || { echo "/quit took ${took}s"; exit 1; }
    exit 0
  '

# What THIS run printed: everything below the last line of the launch command's own echo, which is
# the line carrying the exit-file redirect.
#
# MERGE: it used to be `tail -20` of the whole primary buffer. This script starts the binary TWICE,
# so the earlier session's farewell is still in scrollback, and whether it fell inside those twenty
# lines depended on how many lines the launch command wrapped to — which the merged tree changed by
# adding a `--patch`. "ONE line, not a banner" is a claim about THIS exit, and the window has to say
# so. With no marker on screen at all the old window is used, so the bullet can still fail.
farewell_once() {
  local all last txt n
  all="$(shell-use text | sed "s/[[:space:]]*$//" | grep -v "^$")"
  last="$(printf "%s\n" "$all" | grep -n "quit.exit" | tail -1 | cut -d: -f1)"
  if [ -n "$last" ]; then
    txt="$(printf "%s\n" "$all" | sed -n "$((last + 1)),\$p")"
  else
    txt="$(printf "%s\n" "$all" | tail -20)"
  fi
  [ -n "$txt" ] || { echo "the screen is blank after /quit"; return 1; }
  # `grep -qi "bough\|bye"` was VACUOUS: after the alt screen is left, the primary buffer still
  # carries the shell echo of the launch command, which is `$BOUGH_BIN …` — a path ENDING in
  # `bough`. It passed whether or not `run::farewell()` ever printed. The farewell is one exact
  # string, spelled once in the product; this asserts that string.
  printf "%s\n" "$txt" | grep -qF "bough: bye." || {
    echo "no farewell line on screen after /quit:"; printf "%s\n" "$txt"; return 1; }
  # ONE line, not a banner.
  n="$(printf "%s\n" "$txt" | grep -cF "bough: bye.")"
  [ "$n" -eq 1 ] || { echo "the farewell appears $n times"; printf "%s\n" "$txt"; return 1; }
  return 0
}
export -f farewell_once

t the_farewell_is_one_line_and_the_screen_is_not_blank \
  bash -c 'farewell_once'

tui_close
