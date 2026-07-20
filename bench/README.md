# Harness bench: bough vs Claude Code, same model

Fixed-model A/B harness comparison. Both harnesses run **claude-haiku-4-5** —
a deliberately weak model, because harness gains are largest on weak models
(AHE, arXiv 2604.25850) — over the same task bank, and an oracle `verify.sh`
grades the **final workspace state** (never the transcript's claims).

## Run

```sh
bench/run.sh                 # all tasks, 2 trials each, both harnesses
bench/run.sh -n 4            # 4 trials
bench/run.sh -H bough        # one harness (re-verifying a bough-only edit)
bench/run.sh bugfix-inventory  # one task
```

Results append to `results/results.jsonl` (one JSON line per trial);
`report.py` prints pass rate, median wall clock, output tokens, the
headline number — **cost per solved task** — and a failure taxonomy.
`gap.py` prints the **verification gap**: bough trials whose transcript
claimed success while the oracle failed (execution alignment, the thing
the check contract exists to prevent).

## Failure taxonomy

On a failed trial the runner re-runs the oracle under `bash -x` and buckets
the first failing assertion into `fail_reason`: `timeout`,
`protected-file-modified`, `tests-fail`, `missing-file`, `mutant-not-caught`,
`output-mismatch`, `content-check`, `unclassified`. Read it before treating a
loss as a capability gap — `output-mismatch` is usually a verification bug,
`timeout`/sandbox trouble is a harness bug.

## Harness-edit protocol (AHE-style)

Every harness change ships with a falsifiable prediction, recorded in
`predictions.jsonl` **before** the verifying sweep runs: what failure class it
targets, expected pass-rate movement, expected cost. Verify with
`run.sh -H bough`, then set `outcome` (`confirmed` / `refuted` / `mixed`).
Edits whose predictions fail get reverted — no vibes-driven prompt growth.

## How each side runs headless

- **Claude Code**: `claude -p --output-format json --dangerously-skip-permissions
  --setting-sources project` in a fresh fixture copy. Tokens/cost/turns come
  from the result envelope.
- **bough** (no headless CLI yet): `server.sh` boots an isolated server —
  own port (4599), `BOUGH_DB`/`BOUGH_SHADOW_BASE`/`BOUGH_SUBAGENT_BASE`/
  `BOUGH_SNAPSHOT_BASE` under `bench/state/`, model pinned via `BOUGH_MODEL`
  env — so nothing touches `~/.bough` or the daily-driver server. A trial is:
  POST /sessions (workspace = fixture copy) → per-session net yolo →
  POST message → wait for `turn.finished` on the SSE tail → verify against the
  session's shadow worktree → read tokens from the bench DB.

## Tasks

| task | shape | oracle |
| --- | --- | --- |
| bugfix-inventory | failing test, fix impl not tests | tests green + test file byte-identical |
| feature-topwords | add `--top N` flag to a CLI | exact output incl. tie-break, old behavior intact |
| refactor-rename | rename across 3 files | no old name remains, tests untouched + green |
| rename-precise | rename a method whose name recurs as decoys (dict key, string, unrelated class) | only the real call sites change; a global text replace breaks the decoy test — isolates *semantic* rename (lsp.rename) from text search |
| test-writing | write tests for slugify | tests pass on real impl AND fail on two mutants |
| wiring-stats | new command + registration | exact output, existing commands still work |

Fixtures carry identical `AGENTS.md` and `CLAUDE.md` so neither harness gets
extra guidance. Each trial stages a fresh git-committed fixture copy.

## The 2x2 (model x harness), 2026-07-19

| cell | pass | cost/solved |
| --- | --- | --- |
| haiku-4.5 + bough | 14/16 | $0.042 |
| haiku-4.5 + Claude Code | 16/16 | $0.076 |
| sonnet-5 + bough | 15/16 | $0.060 |
| sonnet-5 + Claude Code | 14/16 | $0.193 |

Two literature results replicate locally: **harness-induced cost variance
(3.2x on sonnet at matched pass rates) exceeds model-induced variance**
(Scaffold Effect), and the **pass-rate ranking reverses across tiers** (CC
wins on haiku, bough on sonnet — CC-on-sonnet walks into the crossmodule
misleading-comment trap that haiku ignores). Report harness+model pairs,
never either alone.

## Comparability caveats

- **Sandbox asymmetry**: bough turns run seatbelt-sandboxed behind the net
  gate (yolo'd); Claude Code runs with permissions skipped and no sandbox.
  That's part of each harness — but check the failure taxonomy before reading
  a bough loss as a capability gap (a sandbox denial is a different bug).
- **Token accounting**: Claude Code's `cost_usd` is provider-reported;
  bough's is computed from cumulative session tokens at Haiku list price
  ($1/$5 per Mtok) with cache reads billed 0.1x and writes 1.25x (from the
  sessions table's cache_read_total/cache_write_total).
- **Budget caps**: Claude Code gets `--max-budget-usd 2`; bough has no cost
  cap (known gap), so both sides rely on the shared 900s wall-clock timeout.
