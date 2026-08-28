#!/usr/bin/env bash
# §17 Phase 4 — the four governance commands, on screen. `/seal`, `/reconsolidate`, `/drift` and
# `/reset` are what this phase makes VISIBLE, and §17's testing policy says every TUI-visible
# behaviour of the phase gets a shell-use bullet.
#
# The ledger is seeded with a lived day BEFORE the binary starts (the `06-catch-up.sh` precedent),
# because governance governs a trajectory that already exists: driving 84 steps through the model
# to create one would make this script a test of the loop.
source "$(dirname "$0")/lib.sh"

[ -n "$BOUGH_LIVE" ] && { skip seal_renders_a_report_in_the_pane "the governance commands are dispatched, not modelled"; exit 0; }

MEMORY_PATCH="$REPO_ROOT/scripts/tui/fixtures/memory.patch.yml"

# `see_any <name-for-the-message> <pattern…>`: the screen carries at least ONE of the patterns.
#
# Every pattern below is text the REPORT writes and nothing else on screen does — `call(s),`,
# `tool entropy`, `from raw evidence`. Matching a word the trajectory also carries (`sealed`,
# `tier`, `thought`) would pass whether or not a report ever reached the pane, which is exactly
# how a vacuous bullet is built.
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

# One boot to CREATE the lane, so the seed has a real chain to extend.
tui_open
tui_start "$MEMORY_PATCH"
wait_for "sol" 20000
tui_quit
tui_close

# The seeded day. Piped, not passed as an argument: the fixture opens with a `--` comment and
# sqlite3 reads a leading `-` as an option.
# The ledger is left in WAL and the schema is not readable from outside until a process has
# opened and closed it cleanly — a `--check` boot does exactly that (the `06-catch-up.sh`
# precedent). Without it `sqlite3` sees a zero-byte file and the seed reports "no such table".
"$BOUGH_BIN" --profile headless --check >/dev/null 2>&1 || true

sqlite3 "$LEDGER" < "$REPO_ROOT/scripts/tui/fixtures/seed-trajectory.sql"

t the_seed_landed \
  bash -c "[ \"\$(sql \"select count(*) from steps where id like 'seed-thought-%';\")\" = 60 ]"

tui_open
tui_start "$MEMORY_PATCH"

tiers_now() { sql "select count(*) from rollups where kind = 'tier';"; }
# The `bash -c` bullets below are a FRESH shell (lib.sh exports its own helpers for the same
# reason); without this the function is silently missing there and the comparison reads as empty.
export -f tiers_now

# ---------------------------------------------------------------------------
# /seal
# ---------------------------------------------------------------------------
before_sealed="$(steps_of 'rollup/sealed')"
before_wakes="$(steps_of 'wake/start')"
before_starts="$(steps_of 'step/start')"

shell-use submit "/seal"
wait_any 60000 "call(s)," "nothing to seal"

t seal_renders_a_report_in_the_pane \
  see_any "the seal report" "call(s)," "nothing to seal"

t seal_appended_rollup_sealed_steps \
  bash -c "for i in \$(seq 1 60); do \
             [ \"\$(steps_of 'rollup/sealed')\" -gt \"$before_sealed\" ] && exit 0; sleep 0.5; done; \
           echo \"rollup/sealed is still \$(steps_of 'rollup/sealed')\"; exit 1"

# §8 and P3-D8: a governance pass is a COMMAND. It calls a model of its own, and it must not
# dispatch a turn on the agent's behalf — no new wake, no new step of the loop's own.
t seal_started_no_wake \
  bash -c "[ \"\$(steps_of 'wake/start')\" = \"$before_wakes\" ] \
        && [ \"\$(steps_of 'step/start')\" = \"$before_starts\" ]"

tiers_after_seal="$(tiers_now)"

# ---------------------------------------------------------------------------
# /reconsolidate
# ---------------------------------------------------------------------------
shell-use submit "/reconsolidate"
wait_any 60000 "contradictions proposed" "nothing was written"

t reconsolidate_renders_a_report \
  see_any "the reconsolidation report" "contradictions proposed" "nothing was written"

# ---------------------------------------------------------------------------
# /drift
# ---------------------------------------------------------------------------
shell-use submit "/drift"
wait_any 30000 "Tool use" "No tool calls"

# ux-visual D-uxv-7: `/drift` is sentences first (`Tool use: …` / `No tool calls …`), numbers
# after (`raw: … entropy=`).
t drift_renders_the_signals \
  see_any "the drift signals" "Tool use" "No tool calls"

# §8: the claim-rejection-rate signal is WIRED and INACTIVE until Phase 5's accept/reject surface
# exists. A surface that quietly showed a made-up number instead would be the §16 failure.
t drift_reports_claim_rejection_as_inactive \
  see_any "the claim-rejection signal" "Claim rejection: not measurable yet"

# ---------------------------------------------------------------------------
# /reset
# ---------------------------------------------------------------------------
before_about="$(steps_of 'about/line')"

shell-use submit "/reset sol"
wait_steps 'about/line' $(( ${before_about:-0} + 1 )) 120

# The about-line's STATE half is rebuilt from raw evidence, and the strip renders it — so the
# ledger half (a NEW `about/line` step) and the screen half (a report in the pane) are both
# asserted. The strip's own text is not diffed: the rail repaints on a timer and a screen diff
# would be a timing test rather than a behaviour one.
t reset_renders_a_report_and_the_strip_about_line_changes \
  bash -c "see_any 'the reset report' 'from raw evidence' \
        && for i in \$(seq 1 60); do \
             [ \"\$(steps_of 'about/line')\" -gt \"$before_about\" ] && exit 0; sleep 0.5; done; \
           echo \"about/line is still \$(steps_of 'about/line')\"; exit 1"

# §8: `/reset` rebuilds the digest and identity; SEALED TIERS ARE UNTOUCHED. Neither re-sealed nor
# deleted — the count is exactly what the seal pass left.
t reset_left_the_tier_count_unchanged \
  bash -c "[ \"\$(tiers_now)\" = \"$tiers_after_seal\" ]"

tui_quit
