#!/usr/bin/env bash
# V2 — a claim decided IN THE TUI. The card renders with three actions, accepting a requirement
# appends a pin, accepting a lane claim BIRTHS A LANE and a rail beside the others, editing pins
# the edited text, rejecting records the reason and births nothing, and a CLICK decides a card —
# because acceptance is Andrey's act and a click is his hand on the keyboard.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip a_claim_card_renders_with_three_actions "the claim seam is decided by hand, not by a model"; exit 0; }

LANES="$REPO_ROOT/scripts/tui/fixtures/many-agents.patch.yml"

# One boot to create the lanes, so the seeded claims land on a chain that exists.
tui_open
tui_start "$LANES"
wait_for "sol" 20000
tui_quit
tui_close

# The ledger is left in WAL and is not readable from outside until a process has opened and closed
# it cleanly; `--check` does exactly that (the `06-catch-up.sh` precedent).
"$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true
sqlite3 "$LEDGER" < "$REPO_ROOT/scripts/tui/fixtures/seed-lanes.sql"

tui_open
tui_start "$LANES"
wait_for "sol" 30000

# The pane opens on the FIRST rail, which is not necessarily the lane these steps were seeded on.
shell-use submit "/focus sol" >/dev/null
wait_for "sol" 10000

# The card. Three hit regions — `claim:<id>:accept|edit|reject` — so all three words are on screen.
t a_claim_card_renders_with_three_actions \
  bash -c 'see "SEEDED-REQUIREMENT" --timeout 20000 \
        && see "accept" --timeout 10000 \
        && see "edit" --timeout 10000 \
        && see "reject" --timeout 10000'

# ---------------------------------------------------------------------------
# accept: a requirement
# ---------------------------------------------------------------------------
accepted_before="$(steps_of 'claim/accepted')"
pins_before="$(steps_of 'pin/set')"

shell-use submit "/accept SEED-CLAIM-REQ"
wait_steps 'claim/accepted' $(( ${accepted_before:-0} + 1 ))

t accept_appends_claim_accepted \
  bash -c "[ \"\$(steps_of 'claim/accepted')\" -gt \"$accepted_before\" ]"

# §3: "accepted requirements are pins".
t an_accepted_requirement_appears_as_a_pin \
  bash -c "[ \"\$(steps_of 'pin/set')\" -gt \"$pins_before\" ]"

# ---------------------------------------------------------------------------
# accept: a lane claim BIRTHS a lane
# ---------------------------------------------------------------------------
shell-use submit "/accept SEED-CLAIM-LANE"
wait_steps 'claim/accepted' $(( ${accepted_before:-0} + 2 ))
wait_for "vega" 20000

# THE RAIL, not the notice band: `/accept` prints `lane born: vega` into the band itself, so
# `see "vega"` was satisfied by the command's own echo whether or not a rail row ever appeared.
# A rail row carries the lane's name AND its state glyph on one line.
t accepting_a_lane_claim_adds_a_rail_row \
  bash -c 'row_with "vega" "○" || row_with "vega" "◐" || row_with "vega" "●"'

t the_new_lane_has_an_agents_row \
  bash -c "[ \"\$(sql \"select count(*) from agents where name = 'vega';\")\" = 1 ]"

# ---------------------------------------------------------------------------
# edit and reject
# ---------------------------------------------------------------------------
# A claim of its own: editing one that was already accepted is refused. The edited text is a real
# SENTENCE: `/edit <claim> <text…>` advertises one, and its schema now admits the rest of the line
# — the argument list used to be capped at two words, so the keyboard half of the
# accept/edit/reject gate could only ever edit a claim to a single token.
shell-use submit "/edit SEED-CLAIM-EDIT citations are how the ledger is read"
wait_steps 'pin/set' $(( ${pins_before:-0} + 1 ))

t edit_pins_the_edited_text \
  bash -c "[ \"\$(sql \"select count(*) from steps where type = 'pin/set' and body like '%citations are how the ledger is read%';\")\" -ge 1 ]"

rejected_before="$(steps_of 'claim/rejected')"
agents_before="$(sql 'select count(*) from agents;')"
shell-use submit "/reject SEED-CLAIM-REJ that is what the wards are for"
wait_steps 'claim/rejected' $(( ${rejected_before:-0} + 1 ))

t reject_records_the_reason_and_births_nothing \
  bash -c "[ \"\$(steps_of 'claim/rejected')\" -gt \"$rejected_before\" ] \
        && [ \"\$(sql \"select count(*) from steps where type = 'claim/rejected' and body like '%that is what the wards are for%';\")\" -ge 1 ] \
        && [ \"\$(sql 'select count(*) from agents;')\" = \"$agents_before\" ]"

# ---------------------------------------------------------------------------
# the click path: `Actor::Andrey`, because a click is Andrey's hand
# ---------------------------------------------------------------------------
tui_quit
tui_close
"$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true
# One more open card, written inline rather than re-running the fixture: the fixture's ids are
# fixed, so a second run of it would fail on `UNIQUE constraint failed: steps.id` and plant nothing.
sqlite3 "$LEDGER" <<'SQL'
INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
SELECT
  'seed-claim-click',
  'lane/sol',
  (SELECT COALESCE(MAX(seq), 0) + 1 FROM steps WHERE traj_id = 'lane/sol'),
  strftime('%Y-%m-%dT%H:%M:%SZ', 'now'),
  'wake:seed',
  'claim/proposed',
  'thought',
  json_object(
    'by', 'sol',
    'claim', 'SEED-CLAIM-CLICK',
    'kind', 'other',
    'title', 'SEEDED-CLICKABLE',
    'body', 'a card decided by a click, not by a command',
    'detail', json_object('kind', 'other')
  ),
  json_array(),
  0;
SQL

tui_open
tui_start "$LANES"
wait_for "sol" 30000
shell-use submit "/focus sol" >/dev/null
wait_for "sol" 10000
# Loudly: a card that is not on screen makes the click bullet below a click on nothing.
t the_clickable_card_is_on_screen \
  see "SEEDED-CLICKABLE" --timeout 20000

clicked_before="$(steps_of 'claim/accepted')"
shell-use mouse click --on-text "[accept]" >/dev/null
wait_steps 'claim/accepted' $(( ${clicked_before:-0} + 1 ))

t a_click_on_accept_decides_the_card \
  bash -c "for i in \$(seq 1 40); do \
             [ \"\$(steps_of 'claim/accepted')\" -gt \"$clicked_before\" ] && exit 0; sleep 0.5; done; \
           echo 'the click appended no claim/accepted'; exit 1"

tui_quit
