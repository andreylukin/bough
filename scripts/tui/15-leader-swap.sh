#!/usr/bin/env bash
# SWAP (§17 Phase 5), on screen — the `leader` SET moves from one agent's scope to another BY A
# PATCH FILE while the binary runs. The five leader tools are offered to `sol`; the patch lands
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

# The one leader tool this script names. Not all five: the row registers them together, so a suite
# that restated the set would go red on a sixth tool rather than on the swap it is about.
PROBE_TOOL="adopt_unsorted"

# `turn_on <agent>`: focus that lane and send it one message, through the surface.
turn_on() {
  shell-use submit "/focus $1" >/dev/null
  shell-use wait idle --timeout 10000 >/dev/null
  shell-use submit "say the whole sentence please" >/dev/null
  shell-use wait idle --timeout 40000 >/dev/null
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
shell-use wait idle --timeout 30000 >/dev/null

t the_leader_tools_are_offered_to_the_first_lane \
  bash -c 'offers sol yes'

pid_before="$(pgrep -f "$BOUGH_BIN" | head -1)"
cp "$REPO_ROOT/scripts/tui/fixtures/leader-elsewhere.patch.yml" "$USER_PATCH"

# The launcher's patch watch is DEBOUNCED, so waiting for the recompose is not the same as
# assuming it happened: this polls until `terra` is offered the tools and FAILS if it never is.
took_over() {
  local i
  # The debounce first, THEN the turn. Polling by driving a turn per attempt would burn the
  # replay transcript's rounds on waiting, and `llm-replay` is strict: an unmatched request is a
  # failure, not a shrug. Two attempts, ten seconds apart, is the whole budget this bullet gets.
  for i in 1 2; do
    sleep 10
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
