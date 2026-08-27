#!/usr/bin/env bash
# V3 — text is never destroyed (phase ux1 §2.3). Four separate paths in the audit deleted something
# the user had typed: a slash line that was not a command (B3), a raw multi-line paste (B4), Esc on
# a draft, and Ctrl+U (M20). The rule this script pins is absolute: nothing the user typed is
# removed by anything except an explicit clear.
#
# The shared replay transcript is enough — every bullet is about the composer, and none of them
# needs a particular answer.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip a_missed_command_keeps_the_sentence "the draft is keys, not a model"; exit 0; }

tui_open
tui_start

# --- B3: `/tmp is where my files are` is not a command, and is not a deletion either. ----------
#
# The audit's own repro line. The persona was answering a question about their filesystem and the
# product ate the sentence because it began with a slash.
shell-use type "/tmp is where my files are"
shell-use press Enter
t a_missed_command_keeps_the_sentence \
  see "/tmp is where my files are" --timeout 10000
t the_miss_says_what_to_do_instead \
  bash -c 'see "/help" --timeout 8000'

# A second, unchanged Enter sends it as an ordinary message (§2.3's `arm_send_as_message`).
shell-use press Enter
t a_second_enter_sends_the_missed_line_as_a_message \
  bash -c '
    shell-use wait idle --timeout 30000 >/dev/null 2>&1 || true
    for i in $(seq 1 40); do
      n="$(sql "select count(*) from steps where type = '"'"'mail/delivered'"'"' and body like '"'"'%where my files are%'"'"';")"
      [ "${n:-0}" -ge 1 ] && exit 0
      sleep 0.5
    done
    echo "the missed line never reached the ledger as a delivered message"
    exit 1
  '

# --- B4: a raw three-line paste is ONE draft and ONE send. ------------------------------------
#
# Written as a REAL bracketed paste — `ESC[200~ … ESC[201~`, which is what a terminal wraps
# pasted text in and what the shell turns into one draft. The shell enables bracketed paste at
# boot (`term::enter`), so this, and not a newline-timing guess, is the path a paste takes; the
# `paste_burst_ms` heuristic is the fallback for terminals that cannot speak it, and it is
# deliberately off while this one can (`run::on_key`), where it could only ever mistake a fast
# typist for a paste.
shell-use write "$(printf '\033[200~first pasted line\nsecond pasted line\nthird pasted line\033[201~')"
sleep 1
t a_raw_three_line_paste_is_one_draft \
  bash -c 'see "first pasted line" --timeout 8000 && see "third pasted line" --timeout 8000'

before_sends="$(sql "select count(*) from steps where type = 'mail/delivered';")"
shell-use press Enter
t a_raw_three_line_paste_is_one_draft_and_one_send \
  bash -c '
    shell-use wait idle --timeout 30000 >/dev/null 2>&1 || true
    sleep 1
    after="$(sql "select count(*) from steps where type = '"'"'mail/delivered'"'"';")"
    delta=$(( ${after:-0} - '"${before_sends:-0}"' ))
    [ "$delta" -le 1 ] || { echo "the paste produced $delta sends, expected 1"; exit 1; }
  '

# --- Esc on a non-empty draft leaves it alone. -------------------------------------------------
shell-use type "a draft Esc must not eat"
shell-use press Escape
t esc_leaves_the_draft \
  see "a draft Esc must not eat" --timeout 8000

# --- Ctrl+U clears the LINE, not one character (M20). -----------------------------------------
shell-use keys "Ctrl+u"
t ctrl_u_clears_the_line \
  see "a draft Esc must not eat" --not --timeout 8000

# --- Up recalls the last sent message, Down gives the live draft back. ------------------------
#
# Read on the COMPOSER's own row, never anywhere on screen: the recalled text is a message that
# was sent, so it is also in the transcript, and `see`/`see --not` over the whole screen would
# pass and fail for reasons that have nothing to do with the draft.
composer_holds() {
  local i
  for i in $(seq 1 30); do
    shell-use text | tail -3 | grep -qF "$1" && return 0
    sleep 0.25
  done
  echo "the composer never held: $1"
  shell-use text | tail -3
  return 1
}
composer_empty() {
  local i
  for i in $(seq 1 30); do
    shell-use text | tail -3 | grep -qF "$1" || return 0
    sleep 0.25
  done
  echo "the composer still holds: $1"
  shell-use text | tail -3
  return 1
}
export -f composer_holds composer_empty

shell-use press Up
t up_recalls_the_last_sent_message \
  bash -c 'composer_holds "third pasted line"'
shell-use press Down
t down_returns_the_live_draft \
  bash -c 'composer_empty "third pasted line"'

# --- Shift+Enter and Alt+Enter insert a newline instead of sending. ---------------------------
sends_before="$(sql "select count(*) from steps where type = 'mail/delivered';")"
shell-use type "line one"
shell-use keys "Shift+Enter"
shell-use type "line two"
t shift_enter_inserts_a_newline \
  bash -c '
    see "line one" --timeout 8000 && see "line two" --timeout 8000 || exit 1
    sleep 1
    after="$(sql "select count(*) from steps where type = '"'"'mail/delivered'"'"';")"
    [ "${after:-0}" -eq '"${sends_before:-0}"' ] || { echo "Shift+Enter sent the draft"; exit 1; }
  '

shell-use keys "Alt+Enter"
shell-use type "line three"
t alt_enter_inserts_a_newline \
  bash -c '
    see "line three" --timeout 8000 || exit 1
    sleep 1
    after="$(sql "select count(*) from steps where type = '"'"'mail/delivered'"'"';")"
    [ "${after:-0}" -eq '"${sends_before:-0}"' ] || { echo "Alt+Enter sent the draft"; exit 1; }
  '

shell-use keys "Ctrl+u"
tui_quit
