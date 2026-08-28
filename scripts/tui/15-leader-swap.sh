#!/usr/bin/env bash
# SWAP (§17 Phase 5), on screen — the `leader` SET moves from one agent's scope to another BY A
# PATCH FILE while the binary runs. The leader tools are offered to `sol`; the patch lands
# without a restart; `sol` is no longer offered them and `terra` is.
#
# The `11-swap-rollups.sh` precedent, one seam over: that swap changed which PLUGIN satisfied a
# key, this one changes WHOSE SCOPE a set of registrations lives in.
#
# "Offered" is read from the LEDGER, not from a pane: `request/header.tools` is the durable record
# of the tool names an agent's request actually carried (§5), and there is no `/tools` pane to
# grep. The screen is what DRIVES each turn; the header is what proves what it was offered.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip the_leader_tools_are_offered_to_the_first_lane "the swap gate is composition, not a model"; exit 0; }

USER_PATCH="$HOME_DIR/bough.patch.yml"

# The one leader tool this script names. Not both: the row registers them together, so a suite
# that restated the set would go red on a third tool rather than on the swap it is about.
PROBE_TOOL="curate"

# `turn_on <agent>`: focus that lane and send it one message, through the surface.
turn_on() {
  local before
  before="$(headers_on "$1")"
  shell-use submit "/focus $1" >/dev/null
  # The notice `/focus` itself raises. The lane NAME is on the rail whether or not the focus moved,
  # so waiting for it waited for nothing and the message below could be sent to the old lane.
  wait_for "focused $1" 10000
  shell-use submit "say the whole sentence please" >/dev/null
  # Waited on the LEDGER, not on the screen. `wait idle` here ran out its whole 40 s on every call
  # — the status line repainted a clock for as long as the wake ran — and the obvious screen
  # replacement is worse than useless: every replayed round says the same sentence, so the answer
  # this turn is about is already on screen from the last one. The new `request/header` is not.
  local i
  for i in $(seq 1 80); do
    [ "$(headers_on "$1")" -gt "${before:-0}" ] && return 0
    sleep 0.5
  done
  return 0
}
export -f turn_on

# The newest `request/header` on a lane, and whether it carried the probe tool.
headers_on() { sql "select count(*) from steps where type = 'request/header' and traj_id = 'lane/$1';"; }
newest_header_has_probe() {
  local n
  n="$(sql "select count(*) from (select body from steps where type = 'request/header' and traj_id = 'lane/$1' order by seq desc limit 1) where body like '%$PROBE_TOOL%';")"
  [ "${n:-0}" -eq 1 ]
}
export -f headers_on newest_header_has_probe
export PROBE_TOOL

# `offers <agent> <yes|no>`: drive one turn on the lane and read the header it left behind. The
# header count must MOVE, or "the newest header does not name the tool" would be a sentence about
# a header from before the patch.
offers() {
  local before after
  before="$(headers_on "$1")"
  turn_on "$1"
  after="$(headers_on "$1")"
  if [ "${after:-0}" -le "${before:-0}" ]; then
    echo "no new request/header on lane/$1: the turn never reached the model"
    return 1
  fi
  if [ "$2" = yes ]; then
    newest_header_has_probe "$1" || { echo "lane/$1 was NOT offered $PROBE_TOOL"; return 1; }
  else
    newest_header_has_probe "$1" && { echo "lane/$1 was STILL offered $PROBE_TOOL"; return 1; }
  fi
  return 0
}
export -f offers

tui_open
tui_start
wait_for "terra" 30000

t the_leader_tools_are_offered_to_the_first_lane \
  bash -c 'offers sol yes'

pid_before="$(pgrep -f "$BOUGH_BIN" | head -1)"
cp "$REPO_ROOT/scripts/tui/fixtures/leader-elsewhere.patch.yml" "$USER_PATCH"

# The launcher's patch watch is DEBOUNCED, so waiting for the recompose is not the same as
# assuming it happened: this polls until `terra` is offered the tools and FAILS if it never is.
took_over() {
  local i
  # The debounce first, THEN the turn. Polling by driving a turn per attempt burns a round of the
  # replay transcript on every attempt, and `llm-replay` is strict: an unmatched request is a
  # failure, not a shrug.
  #
  # MERGE (track B -> Phase 5): SIX attempts, not two. The merged tree is 54 rows where Phase 5's
  # was 40, so a live recompose has more to do, and on a machine running four of these suites at
  # once it did not finish inside the old twenty-second budget — a red bullet about the machine
  # rather than about the swap. `fixtures/llm-replay.patch.yml` grew to sixteen rounds to pay for
  # it. The claim is unchanged: if `terra` is never offered the tool, this still fails.
  #
  # phase ux1 (the suite-speed pass): THREE seconds per attempt, not ten. With `turn_on` waiting on
  # the ledger rather than on a clock, this bullet was re-run against a 3 s window and passed on
  # the FIRST attempt every time — the live recompose really does re-offer the set to the new
  # lane's next wake, and the ten seconds were paying for the 40 s `wait idle` upstream. Six
  # attempts still stand, so a slow machine is still tolerated and a set that never moves still
  # fails.
  for i in 1 2 3 4 5 6; do
    sleep 3
    if offers terra yes >/dev/null 2>&1; then return 0; fi
  done
  echo "the set never moved: terra is still not offered $PROBE_TOOL"
  return 1
}
export -f took_over

t the_patch_lands_without_a_restart \
  bash -c "took_over && [ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

t the_first_lane_no_longer_offers_them \
  bash -c 'offers sol no'

t the_second_lane_does \
  bash -c 'offers terra yes'

# Put it back, so the home this script leaves behind is the tree the bundle describes.
rm -f "$USER_PATCH"
tui_quit
