#!/usr/bin/env bash
# V8 (on screen) — the BRANCH PICKER. `^b` lists the focused agent's branches: a child with an
# `agents` row is a LANE, one without is a FORK (§4), selecting one switches the pane to that
# trajectory, and `Esc` returns to the agent's own chain.
#
# Switching a fork into view is a pane-local trajectory override, never a `FocusRequest`: a fork
# has no agent to focus. That is why the last bullet is about the PANE going back, not about focus.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip the_picker_lists_the_agents_branches "the picker reads the ledger, not a model"; exit 0; }

LANES="$REPO_ROOT/scripts/tui/fixtures/many-agents.patch.yml"

# One boot to create the lanes, then the seed: `traj/fork-of-sol` (a fork — steps and an ancestor
# edge, no `agents` row) beside the lane children a split would make.
tui_open
tui_start "$LANES"
shell-use wait idle --timeout 30000 >/dev/null
tui_quit
tui_close

"$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true
sqlite3 "$LEDGER" < "$REPO_ROOT/scripts/tui/fixtures/seed-lanes.sql"

# A LANE child too, so "labelled differently" has two things to be different about: `lane/bud` has
# an `agents` row of its own and an ancestor edge back to `lane/sol`.
sqlite3 "$LEDGER" <<'SQL'
INSERT INTO steps (id, traj_id, seq, at, wake_id, type, class, body, cites, ignorable)
VALUES ('seed-bud-step', 'lane/bud', 1,
        strftime('%Y-%m-%dT%H:%M:%SZ', 'now'), 'wake:seed', 'thought/text', 'thought',
        json_object('text', 'SEEDED-BUD-CONTENT: the lane child''s own chain', 'step_index', 0),
        json_array(), 0);

INSERT INTO edges (child_traj, parent_traj, at_seq, kind, at)
SELECT 'lane/bud', 'lane/sol',
       (SELECT COALESCE(MAX(seq), 1) FROM steps WHERE traj_id = 'lane/sol'),
       'ancestor', strftime('%Y-%m-%dT%H:%M:%SZ', 'now');

INSERT INTO agents (name, traj_id, routing_refs, wake_classes, model_override, tick_floor, digest_rollup_id)
VALUES ('bud', 'lane/bud', json_array(), json_array(), NULL, NULL, NULL);
SQL

tui_open
tui_start "$LANES"
shell-use wait idle --timeout 30000 >/dev/null

# The pane opens on the FIRST rail, which is not necessarily the lane these steps were seeded on.
shell-use submit "/focus sol" >/dev/null
shell-use wait idle --timeout 10000 >/dev/null

# `^b` is the FOCUS PANE's key, so the pane must hold the keyboard first. `Tab` cycles pane focus
# (`tui-shell::run::cycle_focus`) and the pane has no title line to click on, so the picker is
# opened by cycling until `^b` takes: which position the trajectory pane is in is the shell's
# business, not this script's.
open_picker() {
  local i
  for i in 1 2 3 4; do
    shell-use keys "Ctrl+b" >/dev/null
    shell-use wait idle --timeout 5000 >/dev/null 2>&1 || true
    if shell-use text | grep -qF "traj/fork-of-sol"; then return 0; fi
    shell-use press Tab >/dev/null
    shell-use wait idle --timeout 5000 >/dev/null 2>&1 || true
  done
  echo "the branch picker never opened"
  return 1
}
export -f open_picker

t the_picker_lists_the_agents_branches \
  bash -c 'open_picker && see "lane/bud" --timeout 15000'

# A child WITH an `agents` row is a lane; one without is a fork. Both words are on the picker.
t a_lane_child_and_a_fork_child_are_labelled_differently \
  bash -c 'see "lane" --timeout 10000 && see "fork" --timeout 10000'

# Select the FORK: the pane switches to a trajectory that has no agent at all. The picker is a
# keyboard list (`branches::BranchPicker::on_key`), so selection is Down-until-then-Enter, not a
# click — and the row is found by NAME so the branch ORDER is the picker's business.
select_fork() {
  local i
  for i in $(seq 1 10); do
    if shell-use text | grep -qE "^ *(>|›|\*)? *traj/fork-of-sol"; then break; fi
    shell-use press Down >/dev/null
    shell-use wait idle --timeout 3000 >/dev/null 2>&1 || true
  done
  shell-use press Enter >/dev/null
  shell-use wait idle --timeout 15000 >/dev/null 2>&1 || true
}
export -f select_fork
select_fork
t selecting_a_branch_shows_its_trajectory \
  see "SEEDED-FORK-CONTENT" --timeout 15000

# `Esc` returns the pane to the agent's own chain THROUGH THE PICKER: `branches::on_key` is what
# remembers the chain, and the shell's own `Esc` (focus the composer) is what a pane sees
# otherwise. So the bullet is driven the way the pane implements it — reopen, then Esc.
shell-use keys "Ctrl+b" >/dev/null
shell-use wait idle --timeout 10000 >/dev/null
shell-use press Escape >/dev/null
shell-use wait idle --timeout 10000 >/dev/null
t esc_returns_to_the_agents_own_chain \
  expect_absent "SEEDED-FORK-CONTENT" --timeout 10000

tui_quit
