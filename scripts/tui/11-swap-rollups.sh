#!/usr/bin/env bash
# SWAP (§17 Phase 4) — the `rollups` row's PROVIDER is replaced BY A PATCH FILE while the binary
# runs. `rollups-summarizer` becomes `rollups-none`, which seals nothing and says so; `/seal` then
# reports nothing to do instead of sealing; removing the patch brings the summarizer back. The
# process never restarts.
#
# The `09-swap-search.sh` precedent, one layer deeper: that swap removed a row, this one changes
# which PLUGIN satisfies a key that three other rows inject.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip tiers_are_on_screen_before_the_patch "the swap gate is composition, not a model"; exit 0; }

MEMORY_PATCH="$REPO_ROOT/scripts/tui/fixtures/memory.patch.yml"
USER_PATCH="$HOME_DIR/bough.patch.yml"

see_any() {
  local what="$1"; shift
  local pat
  for pat in "$@"; do
    if shell-use expect text --no-strict "$pat" --timeout 10000 >/dev/null 2>&1; then
      return 0
    fi
  done
  echo "none of [$*] is on screen ($what)"
  return 1
}
export -f see_any

# One boot to create the lane, then the seeded day, then the boot under test.
tui_open
tui_start "$MEMORY_PATCH"
shell-use wait idle --timeout 30000
tui_quit
tui_close

# `seed_day <tag>`: the fixture carries FIXED row ids, so a second seed would fail on
# `UNIQUE constraint failed: steps.id` and silently add nothing. The tag renames the ids (the
# `06-catch-up.sh` precedent). A second day is what gives the RESTORED summarizer something left
# to seal — the first pass seals everything the seal lag allows.
# The ledger is left in WAL and the schema is not readable from outside until a process has
# opened and closed it cleanly — a `--check` boot does exactly that (the `06-catch-up.sh`
# precedent). Without it `sqlite3` sees a zero-byte file and the seed reports "no such table".
"$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true

seed_day() {
  sed -e "s/seed-thought-/seed-thought-$1-/g" -e "s/seed-toolcall-/seed-toolcall-$1-/g" \
      "$REPO_ROOT/scripts/tui/fixtures/seed-trajectory.sql" | sqlite3 "$LEDGER"
}

seed_day one

tui_open
tui_start "$MEMORY_PATCH"
shell-use wait idle --timeout 30000

tiers_now() { sql "select count(*) from rollups where kind = 'tier';"; }
# The `bash -c` bullets below are a FRESH shell (lib.sh exports its own helpers for the same
# reason); without this the function is silently missing there and the comparison reads as empty.
export -f tiers_now

# `seal_until <n>`: run `/seal` until the tier count exceeds <n>, or give up loudly. A pass is
# capped at `max_calls_per_pass`, and the launcher's patch watch is debounced, so "seal once and
# look" is a race in both directions.
seal_until() {
  local want="$1" i
  for i in $(seq 1 12); do
    if [ "$(tiers_now)" -gt "$want" ]; then return 0; fi
    shell-use submit "/seal" >/dev/null
    shell-use wait idle --timeout 60000 >/dev/null
  done
  echo "the tier count is still $(tiers_now), wanted more than $want"
  return 1
}
export -f seal_until

# Real tiers first: a swap that removes something is only observable when the something is there.
t tiers_are_on_screen_before_the_patch seal_until 0

tiers_before="$(tiers_now)"
pid_before="$(pgrep -f "$BOUGH_BIN" | head -1)"

cp "$REPO_ROOT/scripts/tui/fixtures/rollups-none.patch.yml" "$USER_PATCH"

# The launcher's patch WATCH is debounced, so the recompose is not instantaneous. Waiting for it is
# not the same as assuming it happened: the loop below re-runs `/seal` until the stub's refusal is
# on screen, and FAILS if it never is — which is the swap not taking.
took_over() {
  local i
  for i in $(seq 1 10); do
    shell-use submit "/seal" >/dev/null
    shell-use wait idle --timeout 60000 >/dev/null
    if see_any "the stub's refusal" "seals nothing, ever"; then
      return 0
    fi
  done
  echo "the stub never took over: /seal still reports a summarizer pass"
  return 1
}

export -f took_over

t the_stub_row_took_over_without_a_restart \
  bash -c "took_over && [ \"\$(pgrep -f '$BOUGH_BIN' | head -1)\" = \"$pid_before\" ]"

# Whatever the summarizer had sealed before the patch is EXACTLY what stands: the stub added none.
tiers_under_stub="$(tiers_now)"

shell-use submit "/seal"
shell-use wait idle --timeout 60000

t seal_reports_nothing_to_do_under_the_stub \
  bash -c "see_any \"the stub's refusal\" 'seals nothing, ever' \
        && [ \"\$(tiers_now)\" = \"$tiers_under_stub\" ]"

rm -f "$USER_PATCH"

# Back to the summarizer, with a SECOND day beneath the seal lag: a pass that seals another block
# is the proof the provider is the real one again, and it is only available because there is fresh
# raw trajectory to seal.
seed_day two

# Back to the summarizer.
t removing_the_patch_restores_the_summarizer seal_until "$tiers_under_stub"

tui_quit
