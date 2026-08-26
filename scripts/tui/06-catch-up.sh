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
seed_mail() {
  "$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true
  # REDIRECTED, not passed as an argument: the fixture opens with a `--` comment, and sqlite3
  # reads an argument starting with `-` as an option ("Use -help for a list of options").
  sqlite3 "$LEDGER" < "$(dirname "$0")/fixtures/seed-mail.sql"
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
