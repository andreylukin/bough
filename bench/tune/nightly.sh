#!/usr/bin/env bash
# Continuous prompt-iteration entry point: run one tuner campaign, and if it
# produces a champion that survives n=6 confirmation, open an adoption PR.
#
# Designed to be driven by launchd/cron (see bench/tune/com.bough.prompt-tune.plist)
# but is safe to run by hand. It NEVER touches the current working checkout: the
# adoption commit is built in a throwaway `git worktree` off origin/main, so a
# dirty tree (the common dev state here) is left untouched. A human merges the PR —
# that merge is the actual prompt change.
#
# env knobs: HOURS (default 6), TRIALS (default 3), TASKS (default all),
#            PROPOSER_MODEL, NO_PR=1 (build the branch+commit but don't push/PR).
set -euo pipefail

TUNE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$TUNE/../.." && pwd)"
BENCH="$REPO/bench"
cd "$REPO"

CAMPAIGN="nightly-$(date +%F)"
HOURS="${HOURS:-6}"
TRIALS="${TRIALS:-3}"
log() { echo "[nightly $(date +%H:%M:%S)] $*"; }

log "campaign=$CAMPAIGN hours=$HOURS trials=$TRIALS"

# Online ACE half: mine real-session friction into queued candidate deltas, which
# the tuner then races via --seed-online. The reflector uses REFLECTOR_MODEL, or
# claude's strong account default when unset — it is the idea GENERATOR (one cheap
# call, quality matters), separate from the weak-model bench that JUDGES. Skip with
# ONLINE=0, or when there is no daily-driver db to mine.
if [ "${ONLINE:-1}" = "1" ] && [ -e "${BOUGH_DB:-$HOME/.bough/bough.db}" ]; then
  log "online: mining friction -> candidate deltas"
  python3 "$TUNE/online.py" --campaign "$CAMPAIGN" \
    ${REFLECTOR_MODEL:+--model "$REFLECTOR_MODEL"} || log "online mining failed (continuing)"
else
  log "online mining skipped (ONLINE=${ONLINE:-1}, db present=$([ -e "${BOUGH_DB:-$HOME/.bough/bough.db}" ] && echo yes || echo no))"
fi

# shellcheck disable=SC2086 # TASKS is an intentional word-split task list
python3 "$TUNE/tune.py" --hours "$HOURS" --trials "$TRIALS" --campaign "$CAMPAIGN" --seed-online \
  ${PROPOSER_MODEL:+--proposer-model "$PROPOSER_MODEL"} ${TASKS:-}

SUMMARY="$BENCH/results/tune-$CAMPAIGN-summary.json"
[ -f "$SUMMARY" ] || { log "no summary at $SUMMARY — tuner did not finish"; exit 1; }

confirmed="$(jq -r '.confirmed' "$SUMMARY")"
champion="$(jq -r '.champion' "$SUMMARY")"
detail="$(jq -r '.detail' "$SUMMARY")"
hypothesis="$(jq -r '.hypothesis' "$SUMMARY")"

if [ "$confirmed" != "true" ]; then
  log "no adoption-grade champion this campaign ($champion: $detail) — nothing to PR"
  exit 0
fi
log "confirmed champion: $champion — $detail"

# Build the adoption commit in an isolated worktree off the latest origin/main.
git fetch --quiet origin main
BRANCH="prompt-tune/$CAMPAIGN"
WT="$(mktemp -d)/adopt"
git worktree add --quiet -b "$BRANCH" "$WT" origin/main
cleanup() { git worktree remove --force "$WT" 2>/dev/null || true; }
trap cleanup EXIT

# Copy the winning section files (from THIS checkout's variants dir) into the
# worktree's checked-in prompt dir, then re-seed baseline there so a future
# dump/adopt round-trips against the newly adopted text.
"$TUNE/adopt.sh" "$champion" "$WT"

cd "$WT"
if git diff --quiet -- src/supervisor/prompt; then
  log "champion produced no prompt-dir change vs origin/main — skipping PR"
  exit 0
fi

git add src/supervisor/prompt
git commit --quiet -m "prompt: adopt tuned champion ${champion} (${CAMPAIGN})

Confirmed vs baseline: ${detail}
Hypothesis: ${hypothesis}

Auto-opened by bench/tune/nightly.sh. The bench evidence is in
bench/results/tune-${CAMPAIGN}.jsonl and predictions.jsonl. Review the prompt
diff before merging — merging is the actual adoption.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"

if [ "${NO_PR:-}" = "1" ]; then
  log "NO_PR=1 — branch $BRANCH built locally, not pushed"
  trap - EXIT  # keep the worktree so the user can inspect it
  exit 0
fi

git push --quiet -u origin "$BRANCH"
gh pr create --fill --head "$BRANCH" --base main \
  --title "prompt: adopt tuned champion ${champion} (${CAMPAIGN})" \
  --body "Nightly prompt tuner confirmed **${champion}** at n=6.

**Confirmation:** ${detail}
**Hypothesis:** ${hypothesis}

The diff is section \`.md\` files under \`src/supervisor/prompt/\` (read by \`promptOverride()\`). Evidence: \`bench/results/tune-${CAMPAIGN}.jsonl\`, \`bench/predictions.jsonl\`, \`bench/tune/learnings.md\`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)"
log "opened PR for $BRANCH"
