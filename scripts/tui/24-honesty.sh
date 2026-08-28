#!/usr/bin/env bash
# V9 and V10 — feedback and truth (phase ux1 §2.9, §2.10). The findings this script pins are the
# ones where the product told the user something that was not so, or told them nothing at all:
#
#   B5   a file written "in the current directory" was not in the directory the binary launched in
#   M15  a rejected `bough.patch.yml` was invisible: the log knew, the screen did not
#   M21  a drag selection copied with no acknowledgement at all
#   M24  a running turn showed neither a spinner nor an elapsed clock
#   M25  "what can you do" advertised `open_pr` and `spawn_worker`, which are not registered
#   M28  the about-line was a markdown fragment spliced mid-sentence, and did not survive a quit
#
# The cwd bullet is a DISK assertion. B5's whole nature is that the screen said the file was
# written; only the filesystem can refute that.
# The one script that must keep the ANIMATED status line: `a_running_turn_shows_a_spinner_and_an
# _elapsed_clock` is the assertion M32 exists for, and it is asserted here against the shipped
# default. Set BEFORE the source, which is where `lib.sh` reads it.
TUI_STATIC_STATUS=0
source "$(dirname "$0")/lib.sh"

tui_open

# ==============================================================================================
# V10 — the cwd is the launch cwd
# ==============================================================================================
if [ -z "$BOUGH_LIVE" ]; then
  tui_start "$REPO_ROOT/scripts/tui/fixtures/cwd.patch.yml"
  shell-use submit "write the file in the current directory"
  wait_for "landing.txt" 40000

  t_disk a_file_in_the_current_directory_lands_in_the_launch_cwd \
    "$HOME_DIR/work/landing.txt" "the pinned root is the launch cwd"

  t the_file_did_not_land_in_the_checkout \
    bash -c '
      [ -f "'"$REPO_ROOT"'/landing.txt" ] && { echo "the file landed in the repo checkout"; exit 1; }
      cd "'"$REPO_ROOT"'" && [ -z "$(git status --porcelain -- landing.txt)" ]
    '

  t the_status_line_names_the_launch_cwd \
    bash -c '
      # Elided in the MIDDLE, so the LAST component is what a user reads (§2.5).
      see "work" --timeout 15000 || { echo "the status line does not name the launch cwd"; exit 1; }
    '
else
  tui_start
  skip a_file_in_the_current_directory_lands_in_the_launch_cwd "the disk assertion drives a scripted tool call"
  skip the_file_did_not_land_in_the_checkout "the disk assertion drives a scripted tool call"
  t the_status_line_names_the_launch_cwd see "work" --timeout 20000
fi

# ==============================================================================================
# V9 — a running turn is visibly running
# ==============================================================================================
tui_quit
tui_start "$REPO_ROOT/scripts/tui/fixtures/slow.patch.yml"
if [ -n "$BOUGH_LIVE" ]; then
  # The slow fixture is a replay row; live, an ordinary question runs long enough to see.
  tui_quit
  tui_start
  shell-use submit "Write a careful four-paragraph explanation of what a terminal PTY is."
else
  shell-use submit "start something long"
fi

t a_running_turn_shows_a_spinner_and_an_elapsed_clock \
  bash -c '
    for i in $(seq 1 40); do
      txt="$(shell-use text)"
      # The spinner is one of `spinner`'"'"'s braille frames; the elapsed clock is `<digits>s`.
      printf "%s" "$txt" | grep -q "[⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏]" \
        && printf "%s" "$txt" | grep -qE "[0-9]+(\.[0-9])?s" && exit 0
      sleep 0.3
    done
    echo "no spinner and elapsed clock while the turn ran"
    shell-use text | tail -8
    exit 1
  '
shell-use press Escape
sleep 1

# --- M24: a REAL cost, not `—` forever. -------------------------------------------------------
#
# No bullet anywhere asserted an actual dollar amount: `20-frame.sh` accepts `[$]\|—`, and `—` is
# exactly what `Field::Cost` renders when nothing has landed. Only the live half can have a cost,
# because only a real model round appends `usage/round`.
if [ -n "$BOUGH_LIVE" ]; then
  # A turn that RUNS TO COMPLETION: the round above was interrupted, and an interrupted stream
  # need never deliver its `usage` chunk, so there is nothing for `usage/round` to be built from.
  shell-use submit "Reply with the single word: ok."
  shell-use wait idle --timeout 90000 >/dev/null 2>&1 || true
  t a_finished_turn_puts_a_real_cost_on_the_status_line \
    bash -c '
      for i in $(seq 1 40); do
        shell-use text | grep -qE "[$][0-9]+[.][0-9]+" && exit 0
        sleep 0.5
      done
      echo "the status line still shows an unknown cost after a finished turn"
      shell-use text | tail -4
      exit 1
    '
  t the_cost_came_from_a_durable_usage_round \
    bash -c '
      n="$(sql "select count(*) from steps where type = '"'"'usage/round'"'"';")"
      [ "${n:-0}" -ge 1 ] || { echo "no usage/round in the ledger: the number on screen has no fact behind it"; exit 1; }
    '
else
  skip a_finished_turn_puts_a_real_cost_on_the_status_line "a cost needs a real model round"
  skip the_cost_came_from_a_durable_usage_round "a cost needs a real model round"
fi

# ==============================================================================================
# V9 — the copy flash and OSC52 (M21)
# ==============================================================================================
if [ -z "$BOUGH_LIVE" ]; then
  select_drag 40 4 70 4
  t a_drag_select_flashes_copied_and_emits_osc52 \
    bash -c '
      see "copied" --timeout 8000 || { echo "no copied flash after a drag select"; exit 1; }
      for i in $(seq 1 25); do
        payload="$(shell-use get-recording | grep -o "\\u001b]52;c;[A-Za-z0-9+/=]*" | tail -1 | sed "s/.*;c;//")"
        [ -n "$payload" ] && exit 0
        sleep 0.2
      done
      echo "no OSC52 sequence in the recording"
      exit 1
    '

  t the_copy_hint_names_shift_drag \
    bash -c '
      # The escape hatch for a terminal whose own selection the mouse grab would swallow.
      shell-use text | grep -qi "shift" || { echo "no shift-drag hint anywhere in the chrome"; exit 1; }
    '
else
  skip a_drag_select_flashes_copied_and_emits_osc52 "the copy path is mouse and clipboard, not a model"
  skip the_copy_hint_names_shift_drag "asserted in the replay half"
fi

# ==============================================================================================
# V9 — the config watcher is no longer silent (M15)
# ==============================================================================================
#
# A BAD patch first: the launcher's watch rejects it and logs why, and the audit's finding is that
# the screen never said so. The notice has to carry the LOG'S OWN WORDS, not a generic apology.
write_patch <<'YML'
entries:
  tui.strip:
    config:
      width: 0
YML

t a_rejected_patch_says_so_on_screen_with_the_logs_words \
  bash -c '
    for i in $(seq 1 30); do
      txt="$(shell-use text)"
      printf "%s" "$txt" | grep -qi "reject\|invalid\|refused\|not applied" \
        && printf "%s" "$txt" | grep -q "tui.strip\|width" && exit 0
      sleep 0.5
    done
    echo "the rejected patch left no notice naming the row and the reason"
    shell-use text | tail -10
    exit 1
  '

# A GOOD one: the same surface says it reloaded.
# No `gutter` here: the ux1 review moved the one gutter knob to `tui.gutter`, so a strip patch
# carrying it is REJECTED (unknown field) — which is the previous check's case, not this one's.
write_patch <<'YML'
entries:
  tui.strip:
    config:
      width: 30
      show_about: true
      about_lines: 2
      collapse_cols: 100
      min_width: 22
      max_width: 40
YML

# MERGE: 45 seconds, not 15. The watch is DEBOUNCED and then the whole tree recomposes; the merged
# tree is 54 rows where this bullet was written against 40, and on a loaded machine the notice had
# not been painted when the old window closed. The claim is unchanged — a good patch SAYS it
# reloaded — and a tree that never says so still fails.
t a_good_patch_says_reloaded \
  bash -c '
    for i in $(seq 1 90); do
      shell-use text | grep -qi "reload" && exit 0
      sleep 0.5
    done
    echo "a good patch reloaded with no notice at all"
    exit 1
  '
clear_patch

# ==============================================================================================
# V10 — capability honesty (M25) and the about-line (M28)
# ==============================================================================================
if [ -n "$BOUGH_LIVE" ]; then
  shell-use submit "What can you do? List only the tools you actually have."
  shell-use wait idle --timeout 90000
  t the_capability_answer_names_no_tool_that_is_not_registered \
    bash -c '
      txt="$(shell-use text)"
      for phantom in "merge_pr" "deploy_to_production"; do
        printf "%s" "$txt" | grep -q "$phantom" && {
          echo "the answer advertises $phantom, which no Provider registers"; exit 1; }
      done
      exit 0
    '
else
  # The replay half cannot ask a model, so it asserts the same fact one layer down: the identity
  # band the assembler writes into the request names only the registered tools.
  t the_capability_answer_names_no_tool_that_is_not_registered \
    bash -c '
      body="$(sql "select group_concat(body) from steps where type = '"'"'request/header'"'"';")"
      [ -n "$body" ] || { echo "no request/header in the ledger to read the identity band from"; exit 1; }
      # `spawn_worker` IS mounted in this profile (`tool.spawn_worker` in `bough-base.yml`), so
      # naming it is honest. The phantoms are the ones no row registers anywhere.
      #
      # MERGE (track B -> Phase 5): open_pr came OFF this list. It was a phantom when M25 was
      # written; the actions-github row of track B registers it, along with push_to_pr and
      # draft_ticket, so the band naming it is now the honest half of the claim and the bullet was
      # failing on a tool that exists. The claim is unchanged: a name NO row registers.
      for phantom in "deploy_to_production" "send_email" "merge_pr"; do
        printf "%s" "$body" | grep -q "$phantom" && {
          echo "the identity band names $phantom with no Provider mounted"; exit 1; }
      done
      # …and the band says what it DOES have, rather than nothing at all (M25).
      #
      # `grep -q tools` could not fail: `RequestHeader` has ALWAYS carried a `tools` field, so the
      # word is in every header body whatever `identity_section` wrote — and the shell collapsed
      # the quoting to the bare word anyway. The falsifiable form parses the body and asserts the
      # LIST is non-empty, which an empty `"tools": []` fails.
      printf "%s" "$body" | python3 -c "
import json, sys
raw = sys.stdin.read()
objs, depth, start = [], 0, None
for i, ch in enumerate(raw):
    if ch == chr(123):
        if depth == 0: start = i
        depth += 1
    elif ch == chr(125):
        depth -= 1
        if depth == 0 and start is not None:
            objs.append(raw[start:i+1]); start = None
assert objs, chr(34) + chr(34)
h = json.loads(objs[-1])
tools = h.get(chr(116) + chr(111) + chr(111) + chr(108) + chr(115))
assert isinstance(tools, list) and tools
print(len(tools))
" || exit 1
      exit 0
    '
fi

t the_about_line_is_one_sentence_before_and_after_a_relaunch \
  bash -c '
    # The rail's about-line: an INDENTED continuation row under an agent name, in the rail's own
    # columns. It is not a sentence with a full stop — `one_sentence` clips on a word boundary
    # with an ellipsis — so it is found by WHERE it is drawn, not by how it ends.
    about_line() {
      shell-use text | cut -c1-34 | python3 -c "
import sys
for r in sys.stdin.read().split(chr(10)):
    if r.startswith(\"  \") and r.strip() and not r.strip().startswith(\"intent\"):
        print(r.strip()); break
"
    }
    line="$(about_line)"
    [ -n "$line" ] || { echo "no about-line on screen"; exit 1; }
    printf "%s" "$line" | grep -q "\*\*\|\`\|^#" && { echo "the about-line still carries markdown: $line"; exit 1; }
    n=$(printf "%s" "$line" | grep -o "\." | wc -l | tr -d " ")
    [ "${n:-0}" -le 1 ] || { echo "the about-line is more than one sentence: $line"; exit 1; }
    exit 0
  '

# The ledger is checkpointed on shutdown, so nothing is stranded in a WAL.
tui_quit
t the_shutdown_left_no_wal_over_a_page \
  bash -c '
    wal="'"$LEDGER"'-wal"
    [ -f "$wal" ] || exit 0
    size="$(wc -c < "$wal" | tr -d " ")"
    [ "${size:-0}" -le 4096 ] || { echo "the WAL is $size bytes after shutdown: no checkpoint ran"; exit 1; }
  '

# ==============================================================================================
# V10 — a relaunch restores the FULL history, and the about-line survives it (M28)
# ==============================================================================================
#
# The audit's finding was a relaunch into an empty transcript. The checkpoint above proves the
# rows are on disk; this proves the binary reads them back and DRAWS them — every turn of the
# session, not just the last one. `Home` is the focus-independent jump to the oldest row, so the
# FIRST turn of the session (the cwd fixture's, three restarts ago) is what is asserted.
tui_start "$REPO_ROOT/scripts/tui/fixtures/slow.patch.yml"

# The two halves type DIFFERENT prompts (a replay row cannot be asked a question, and a live model
# cannot be handed one), so the restored transcript is asserted against whatever THIS half sent.
if [ -n "$BOUGH_LIVE" ]; then
  FIRST_TURN="four-paragraph"     # the PTY question, the live half's first submission
  LAST_TURN="What can you do"     # the capability question, its last
else
  FIRST_TURN="write the file in the current directory"
  LAST_TURN="start something long"
fi

t the_about_line_is_one_sentence_after_the_relaunch \
  bash -c '
    see "'"$LAST_TURN"'" --timeout 20000 >/dev/null 2>&1 || true
    about() {
      shell-use text | cut -c1-34 | python3 -c "
import sys
for r in sys.stdin.read().split(chr(10)):
    if r.startswith(\"  \") and r.strip() and not r.strip().startswith(\"intent\"):
        print(r.strip()); break
"
    }
    line=""
    for i in $(seq 1 30); do
      line="$(about)"
      [ -n "$line" ] && break
      sleep 0.5
    done
    [ -n "$line" ] || { echo "no about-line on screen after the relaunch"; exit 1; }
    printf "%s" "$line" | grep -q "\*\*\|\`" && { echo "markdown in the restored about-line: $line"; exit 1; }
    n=$(printf "%s" "$line" | grep -o "\." | wc -l | tr -d " ")
    [ "${n:-0}" -le 1 ] || { echo "the restored about-line is more than one sentence: $line"; exit 1; }
    exit 0
  '

t a_quit_then_relaunch_restores_every_turn \
  bash -c '
    see "'"$LAST_TURN"'" --timeout 20000 \
      || { echo "the last turn did not come back after a relaunch"; exit 1; }
    shell-use press Home
    sleep 0.5
    for i in $(seq 1 20); do
      shell-use text | grep -q "'"$FIRST_TURN"'" && exit 0
      shell-use press Home
      sleep 0.3
    done
    echo "the FIRST turn of the session is not in the restored transcript"
    shell-use text | tail -12
    exit 1
  '

tui_quit

tui_close
