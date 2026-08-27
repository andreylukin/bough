#!/usr/bin/env bash
# V4 (dormancy, on screen), V8 (click-to-focus — the check Phase 3 DEFERRED because there was one
# agent) and the FIELD BUG from Andrey's first live run: consecutive thought/text rows of ONE step
# must render as one flowing paragraph.
#
# Three lanes, so "the strip shows many agents" is a sentence about a population.
source "$(dirname "$0")/lib.sh"

LANES="$REPO_ROOT/scripts/tui/fixtures/many-agents.patch.yml"

# `wakes_on <traj>` — how many wakes ever opened on a trajectory.
wakes_on() { sql "select count(*) from steps where type = 'wake/start' and traj_id = '$1';"; }
delivered_on() { sql "select count(*) from steps where type = 'mail/delivered' and traj_id = '$1';"; }
export -f wakes_on delivered_on

# `focus_composer`: put the keyboard back where a user's next keystroke is meant to go. The
# composer's own placeholder is the anchor, so this stays true if the layout moves.
focus_composer() {
  shell-use mouse click --on-text "message, or" >/dev/null 2>&1 || true
  shell-use wait idle --timeout 5000 >/dev/null 2>&1 || true
}
export -f focus_composer

# `one_line_with <text…>`: some SINGLE screen row carries all of it. `grep` over `shell-use text`
# rather than `see`, because the whole question here is whether a wrapped paragraph is ONE row's
# worth of flowing text or two stacked rows — and `see` cannot tell those apart.
one_line_with() {
  local i
  for i in $(seq 1 60); do
    if shell-use text | grep -qF "$1"; then return 0; fi
    sleep 0.5
  done
  echo "no single screen row contains: $1"
  return 1
}
export -f one_line_with

# ---------------------------------------------------------------------------
# the population
# ---------------------------------------------------------------------------
# One boot to create the three lanes, then a MARKER step on `terra`'s chain: the focus pane's
# title is the word `trajectory` and carries no lane name, so "the pane followed the click" can
# only be read off the CONTENT it is showing.
tui_open
tui_start "$LANES"
shell-use wait idle --timeout 30000 >/dev/null
tui_quit
tui_close

"$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true
sqlite3 "$LEDGER" <<'SQL'
INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT 'seed-terra-marker', 'lane/terra',
       (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/terra'),
       strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'wake:seed', 'thought/text', 'thought',
       json_object('text', 'SEEDED-TERRA-ONLY: this line exists on no other lane', 'step_index', 0),
       json_array(), 0;
SQL

tui_open
tui_start "$LANES"
shell-use wait idle --timeout 30000 >/dev/null

t three_rails_render_with_their_glyphs \
  bash -c 'see "sol" --timeout 20000 && see "terra" --timeout 20000 && see "luna" --timeout 20000'

# ---------------------------------------------------------------------------
# the field bug: ONE paragraph, not two stacked lines
# ---------------------------------------------------------------------------
if [ -z "$BOUGH_LIVE" ]; then
  shell-use type "say the whole sentence please"
  shell-use press Enter

  # The two chunks are ONE row, and — the half that actually catches the field bug — the first
  # fragment is never left standing as a row of its own beside the tail.
  #
  # HONESTY: this does NOT pin the mid-stream moment. `one_line_with` polls for up to 30s, by
  # which time the durable `thought/text` steps have landed, so an on-screen bullet named "while
  # streaming" would assert exactly the same state as the one below it. The mid-stream rule (the
  # renderer's `trailing_text` clause) is pinned purely, in `tui-focus/tests/stream.rs`.
  t a_two_chunk_answer_renders_as_one_paragraph \
    one_line_with "the first fragment and the rest of it"
  t the_first_fragment_is_never_a_row_of_its_own \
    bash -c 'no_row_is_exactly "the first fragment"'

  shell-use wait idle --timeout 30000 >/dev/null
  t and_still_one_paragraph_after_the_step_lands \
    one_line_with "the first fragment and the rest of it"

  # The third bullet — that the join is a RENDER and not a quietly merged ledger — is asserted
  # after the reboot below, on a PLANTED pair. The replay fixture cannot carry it: the loop's
  # text flush folds its two chunks into ONE durable `thought/text` step, so "the ledger still
  # holds two" would be false here for a reason that has nothing to do with the render.
else
  skip a_two_chunk_answer_renders_as_one_paragraph "the live half cannot pin a chunk boundary"
  skip the_first_fragment_is_never_a_row_of_its_own "the live half cannot pin a chunk boundary"
  skip and_still_one_paragraph_after_the_step_lands "the live half cannot pin a chunk boundary"
  skip the_ledger_still_holds_two_thought_text_steps "the live half cannot pin a chunk boundary"
fi

# Click-to-focus, AFTER the turn: a click moves the pane focus off the composer, and a message
# typed afterwards goes to the pane under the pointer rather than into the composer. The order
# here is the order a user's hands take, not an arbitrary one.
#
# The rail row is addressed BY ITS NAME rather than by a computed cell, so the bullet keeps
# meaning if the rail's layout changes.
shell-use mouse click --on-text "terra" >/dev/null
shell-use wait idle --timeout 10000 >/dev/null
t a_click_on_the_second_rail_focuses_it \
  see "terra" --timeout 10000
t the_focus_pane_follows_the_click \
  see "SEEDED-TERRA-ONLY" --timeout 20000

shell-use mouse click --on-text "sol" >/dev/null
shell-use wait idle --timeout 10000 >/dev/null
t a_click_back_returns_to_the_first \
  expect_absent "SEEDED-TERRA-ONLY" --timeout 10000

# ---------------------------------------------------------------------------
# dormancy, on screen
# ---------------------------------------------------------------------------
# Back to the composer first: the click bullets above left the pane focus on the strip, and a
# command submitted then goes to the pane under the pointer.
focus_composer
# A REAL sentence. `/sleep <agent> [reason…]` advertises a reason, and its schema now admits the
# rest of the line as one: the arg list used to be capped at two words, so every reason this suite
# could give was a single hyphenated token.
shell-use submit "/sleep luna nothing to do this week"
shell-use wait idle --timeout 10000 >/dev/null

# `◌` is the glyph and `dormant` is the word (§11: the strip carries both, so a terminal without
# the glyph still says which state a rail is in). The assertion is on LUNA'S RAIL ROW — the name
# and the glyph on ONE line. Asserting `see "dormant"` alone would be satisfied by the command's
# own echo in the notice band, whether or not the rail ever changed.
t a_dormant_lane_shows_the_dormant_glyph \
  bash -c 'row_with "luna" "◌"'
t and_the_word_beside_it \
  bash -c 'row_with "luna" "dormant"'
t the_reason_is_the_whole_sentence \
  bash -c "[ \"\$(sql \"select count(*) from steps where type = 'agent/dormancy' and body like '%nothing to do this week%';\")\" -ge 1 ]"

tui_quit
tui_close

# Mail ARRIVES for the sleeping lane — queued by a previous process, exactly as `seed-mail.sql`
# queues it, and retargeted from `lane/sol` to `lane/luna`.
"$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true
sed -e "s/lane\/sol/lane\/luna/g" \
    -e "s/seed-mail-delivered/seed-mail-delivered-luna/g" \
    -e "s/seed-inbox-spliced/seed-inbox-spliced-luna/g" \
    -e "s/seed-message-1/seed-message-luna/g" \
    -e "s/seed:mail:1/seed:mail:luna/g" \
    "$REPO_ROOT/scripts/tui/fixtures/seed-mail.sql" | sqlite3 "$LEDGER"

# A PLANTED pair: two `thought/text` steps of ONE step index, one wake, split mid-sentence. This
# is the shape the field bug produced on screen ("I'll run that" over " shell command for you.")
# and the shape the render must join.
sqlite3 "$LEDGER" <<'SQL'
INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT 'seed-join-a', 'lane/luna',
       (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/luna'),
       strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'wake:seed-join', 'thought/text', 'thought',
       json_object('text', 'PLANTED-JOIN: I''ll run that', 'step_index', 7), json_array(), 0;

INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT 'seed-join-b', 'lane/luna',
       (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/luna'),
       strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'wake:seed-join', 'thought/text', 'thought',
       json_object('text', ' shell command for you.', 'step_index', 7), json_array(), 0;
SQL

luna_wakes_before="$(wakes_on lane/luna)"
export luna_wakes_before

tui_open
tui_start "$LANES"
shell-use wait idle --timeout 30000 >/dev/null

# The planted pair renders as ONE flowing paragraph…
if [ -z "$BOUGH_LIVE" ]; then
  t the_planted_pair_renders_as_one_paragraph \
    one_line_with "PLANTED-JOIN: I'll run that shell command for you."
  # …while the LEDGER still holds two. That is what keeps the fix a render join rather than a
  # quietly merged ledger.
  t the_ledger_still_holds_two_thought_text_steps \
    bash -c "[ \"\$(sql \"select count(*) from steps where type = 'thought/text' and wake_id = 'wake:seed-join';\")\" -eq 2 ]"
else
  skip the_planted_pair_renders_as_one_paragraph "the live half plants nothing"
  skip the_ledger_still_holds_two_thought_text_steps "the live half plants nothing"
fi

# The whole boot: the roster came up, catch-up asked, and the admission point said no.
t a_dormant_lane_runs_no_wake_while_mail_arrives \
  bash -c "[ \"\$(delivered_on lane/luna)\" -ge 1 ] \
        && [ \"\$(wakes_on lane/luna)\" = \"$luna_wakes_before\" ]"

# …and reactivation drains the backlog in ONE wake (§5's standing invariant).
shell-use submit "/resume luna"
drained_in_one() {
  local i
  for i in $(seq 1 60); do
    if [ "$(wakes_on lane/luna)" -eq "$(( luna_wakes_before + 1 ))" ]; then return 0; fi
    if [ "$(wakes_on lane/luna)" -gt "$(( luna_wakes_before + 1 ))" ]; then
      echo "the backlog took $(( $(wakes_on lane/luna) - luna_wakes_before )) wakes, not one"
      return 1
    fi
    sleep 0.5
  done
  echo "the lane never woke: still $(wakes_on lane/luna) wakes"
  return 1
}
export -f drained_in_one
t waking_it_drains_the_backlog_in_one_wake drained_in_one

tui_quit
