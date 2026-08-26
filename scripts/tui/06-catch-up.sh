#!/usr/bin/env bash
# V6 — §5's catch-up at launch, with the TUI as the lid-open proxy. The ledger is SEEDED with
# queued mail before the binary starts; after boot exactly one new `wake/start` exists, and the
# wake consumed the seeded seqs. Then the same boot with an empty inbox produces no wake at all.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip queued_mail_at_boot_produces_exactly_one_catch_up_wake_per_agent "catch-up is a seam behaviour, not a model one"; exit 0; }

# First boot: create the lane and leave it idle, so the second boot has a real chain to resume.
tui_open
tui_start
shell-use wait idle --timeout 20000
tui_quit
tui_close

# Seed queued mail directly into the ledger: a `mail/delivered` step and the `inbox/spliced` that
# carries it, exactly as `Agent::deliver` writes the pair.
# `seed_mail [tag]`: the fixture carries FIXED row ids, so a second seed in one script would fail
# on `UNIQUE constraint failed: steps.id` and silently queue nothing. The tag renames the ids (and
# only the ids: the subject the bullets assert on stays `SEEDED-CATCH-UP-MAIL`).
seed_mail() {
  local tag="${1:-1}"
  "$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true
  # PIPED, not passed as an argument: the fixture opens with a `--` comment, and sqlite3
  # reads an argument starting with `-` as an option ("Use -help for a list of options").
  sed -e "s/seed-mail-delivered/seed-mail-delivered-$tag/g" \
      -e "s/seed-inbox-spliced/seed-inbox-spliced-$tag/g" \
      -e "s/seed-message-1/seed-message-$tag/g" \
      -e "s/seed:mail:1/seed:mail:$tag/g" \
      "$(dirname "$0")/fixtures/seed-mail.sql" | sqlite3 "$LEDGER"
}

before="$(steps_of 'wake/start')"
seed_mail

tui_open
tui_start
shell-use wait idle --timeout 30000
tui_quit

after="$(steps_of 'wake/start')"
t queued_mail_at_boot_produces_exactly_one_catch_up_wake_per_agent \
  bash -c "[ \$(( $after - $before )) -eq 1 ]"

# The wake does NOT consume the seeded seqs themselves: delivering a queued message at the start
# of a wake appends a FRESH `mail/delivered` step, and that is the seq `wake/end.consumed` names.
# So the bullet is proved the way it is actually true — the consumed range covers a delivery
# carrying the seeded subject.
consumed_the_seeded_mail() {
  local from to n
  from="$(sql "select json_extract(body, '\$.consumed[0].from') from steps where type = 'wake/end' order by seq desc limit 1;")"
  to="$(sql "select json_extract(body, '\$.consumed[0].to') from steps where type = 'wake/end' order by seq desc limit 1;")"
  if [ -z "$from" ] || [ -z "$to" ]; then
    echo "the newest wake/end consumed nothing"
    return 1
  fi
  n="$(sql "select count(*) from steps where type = 'mail/delivered' and seq between $from and $to and body like '%SEEDED-CATCH-UP-MAIL%';")"
  if [ "${n:-0}" -ge 1 ]; then
    return 0
  fi
  echo "the consumed range $from..$to covers no delivery of SEEDED-CATCH-UP-MAIL"
  return 1
}

t the_catch_up_wake_consumed_the_queued_mail consumed_the_seeded_mail

tui_close

# A boot with nothing queued wakes nothing at all.
quiet_before="$(steps_of 'wake/start')"
tui_open
tui_start
shell-use wait idle --timeout 30000
tui_quit
t an_empty_inbox_produces_no_wake_at_all \
  bash -c "[ \"\$(steps_of 'wake/start')\" = \"$quiet_before\" ]"

tui_close

# ---------------------------------------------------------------------------
# "per agent", with more than one agent.
#
# The shipped bundle bootstraps `sol` alone, so the three bullets above prove "exactly one" and
# "none when nothing is queued" on a roster of ONE — where per-agent and in-total are the same
# sentence. A second lane separates them: mail is queued on `sol` only, and the boot must wake
# `sol` exactly once and `terra` not at all.
TWO="$HOME_DIR/two-lanes.yml"
cat > "$TWO" <<YML
entries:
  residents:
    config:
      bootstrap: [sol, terra]
      traj_prefix: "lane/"
      resume_all: true
      catch_up: true
YML

# Boot once with both lanes and nothing queued: this is what creates `terra`.
tui_open
tui_start "$TWO"
shell-use wait idle --timeout 30000
tui_quit
tui_close

wakes_on() { sql "select count(*) from steps where type = 'wake/start' and traj_id = '$1';"; }
sol_before="$(wakes_on lane/sol)"
terra_before="$(wakes_on lane/terra)"
t both_lanes_exist_after_the_two_agent_boot \
  bash -c "[ \"\$(sql \"select count(*) from agents where traj_id = 'lane/terra';\")\" = 1 ]"

seed_mail two   # queues one message on lane/sol ONLY

tui_open
tui_start "$TWO"
shell-use wait idle --timeout 30000
tui_quit

sol_after="$(wakes_on lane/sol)"
terra_after="$(wakes_on lane/terra)"
t exactly_one_catch_up_wake_on_the_agent_that_had_mail \
  bash -c "[ \$(( ${sol_after:-0} - ${sol_before:-0} )) -eq 1 ]"
t no_catch_up_wake_on_the_agent_that_had_none \
  bash -c "[ \$(( ${terra_after:-0} - ${terra_before:-0} )) -eq 0 ]"
