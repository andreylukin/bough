#!/usr/bin/env bash
# §11 "Truth on demand" (the conversation brief, 2026-08-31) — the three depths of the focus pane.
#
# The DATA behind every depth is asserted in `plugins/tui-focus` (`context::tests`); what this
# script owns is the SURFACE: the pane rests as a chat with no truth chrome, the standing head
# line sits in the scrollback rather than a pinned region, typing surfaces the fold line and
# erasing the draft retracts it, `^p` pins the full context view with its counts and footer, and
# `^p` again returns the chat with the keyboard still in the composer.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && {
  skip_all "the depths are a lens on the assembly, not on a model" \
    the_chat_rests_without_truth_chrome \
    the_standing_head_line_rides_the_scrollback \
    typing_surfaces_the_fold_line \
    erasing_the_draft_retracts_the_fold_line \
    ctrl_p_pins_the_full_context_view \
    ctrl_p_again_returns_the_chat \
    the_keys_never_left_the_composer
  exit 0
}

tui_open
tui_start

# One finished turn, so the assembly has a verbatim tail to speak about.
shell-use type "say the whole sentence please"
shell-use press Enter
see "the first fragment and the rest of it" --timeout 30000 >/dev/null \
  || { echo "precondition: the replay answer never landed"; tui_close; exit 1; }
see "idle" --timeout 30000 >/dev/null \
  || { echo "precondition: the wake never closed"; tui_close; exit 1; }

# The resting state is a CHAT: no fold line, no counts, no footer.
chat_is_quiet() {
  expect_absent "verbatim" --timeout 1 || return 1
  expect_absent "rebuilt" --timeout 1 || return 1
  expect_absent "unconsumed" --timeout 1
}
t the_chat_rests_without_truth_chrome chat_is_quiet

# The standing block is not a pinned region here: the folded head line ("identity · fixed · Nk")
# is simply the top of the scrollback, on screen while the whole trajectory fits.
t the_standing_head_line_rides_the_scrollback \
  see "fixed" --timeout 10000

# The PEEK: a draft in the composer annotates the pane with the fold line.
shell-use type "hi"
t typing_surfaces_the_fold_line \
  see "verbatim from here" --timeout 10000

shell-use press Backspace
shell-use press Backspace
t erasing_the_draft_retracts_the_fold_line \
  bash -c '
    for i in $(seq 1 20); do
      shell-use text | grep -q "verbatim from here" || exit 0
      sleep 0.3
    done
    echo "the fold line outlived the draft"
    exit 1
  '

# `^p` pins the FULL view: the tail band with its count, and the footer.
shell-use press "Ctrl+p"
t ctrl_p_pins_the_full_context_view \
  bash -c '
    for i in $(seq 1 30); do
      shell-use text | grep -q "recent steps" && shell-use text | grep -q "rebuilt" && exit 0
      sleep 0.3
    done
    echo "^p never showed the full context view"
    exit 1
  '

shell-use press "Ctrl+p"
t ctrl_p_again_returns_the_chat \
  bash -c '
    for i in $(seq 1 20); do
      shell-use text | grep -q "rebuilt" || exit 0
      sleep 0.3
    done
    echo "the footer outlived the pin"
    exit 1
  '

# `^p` is a lens, not a focus move: the next keystrokes still land in the composer.
shell-use type "zz"
t the_keys_never_left_the_composer \
  see "zz" --timeout 10000

tui_quit
