# Harness bench: bough vs Claude Code, same model

## Overnight prompt tuner (bench/tune/)

`bench/tune/tune.py --hours 8 --trials 3` evolves the system prompt against
this bench, unattended: propose a variant (`claude -p`, contract in
`tune/proposer.md`) → pre-register its falsifiable prediction (variant
`meta.json`, then `predictions.jsonl`) → race it (`run.sh -H bough` with
`BOUGH_PROMPT_DIR` pointing at the variant's section files — no source edits).
Night-2 loop (each mechanism mapped to a night-1 failure, see tune.py's
docstring): failed-trial transcripts from the bench DB feed the proposer
(GEPA-style reflection); challengers screen at n=1 before earning full trials
(TRIPLE racing); `tune/learnings.md` persists verdicts across campaigns; a
task-variance ledger downweights coin-flip tasks in the paired promotion rule;
every 3rd proposal merges two Pareto-frontier variants; and the night's final
champion must survive an n=6 confirmation vs baseline before the report calls
it promoted. Guards: +5% growth cap (prompt-dilution lesson), campaign-scoped
results files (`results/tune-<campaign>.jsonl` — different trial counts never
pool). Morning after: `bench/tune/report.py`, then adopt a winner by porting
its section files into `src/supervisor/prompt.ts` and re-verifying with a
normal sweep. `variants/baseline/` is dumped from the built-ins by
`tune/dump-prompt.ts`; reseed it after any prompt.ts change.

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
| fanout-bugs | 4 INDEPENDENT modules, each with a distinct bug | all four suites green + tests untouched — decomposable, so it rewards parallel delegation; grade the *how* with orch_metrics.py |
| fanout-heavy | 6 INDEPENDENT modules (~265 lines), each with a non-obvious bug needing real reading | all six suites green + tests untouched — the heavy fan-out; still solved serially (~12k parent-ctx), so even this is below the delegation threshold |
| feature-topwords | add `--top N` flag to a CLI | exact output incl. tie-break, old behavior intact |
| refactor-rename | rename across 3 files | no old name remains, tests untouched + green |
| rename-precise | rename a method whose name recurs as decoys (dict key, string, unrelated class) | only the real call sites change; a global text replace breaks the decoy test — isolates *semantic* rename (lsp.rename) from text search |
| test-writing | write tests for slugify | tests pass on real impl AND fail on two mutants |
| wiring-stats | new command + registration | exact output, existing commands still work |

Fixtures carry identical `AGENTS.md` and `CLAUDE.md` so neither harness gets
extra guidance. Each trial stages a fresh git-committed fixture copy.

## Orchestration metrics (subagents & background tasks)

Final pass/fail hides *how* the agent works — ClawArena-Team found leaderboard
scores cluster in a ~10pt band while orchestration behaviors diverge >10x. So to
test and iterate on delegation and background-task use, grade the trajectory, not
just the end state:

```sh
python3 bench/orch_metrics.py <since_ts_seconds> [task]
```

Per trial it reports, from `state/bough.db`: **delegated** (subagents spawned),
**parallel** (in-program `Promise.all` / multiple `agent()`/`spawn()`), **bg-used**
(`bashBg`/`bashWait`/auto-background), **polled** (the sleep/until anti-pattern),
and **parent-ctx** (the parent's context tokens — delegation should keep it lean).

The iteration loop is the same AHE protocol: record a prediction, change the
guidance or mechanism, re-run a task that *rewards* the behavior (`fanout-bugs`),
compare adoption + Pareto (pass/cost/wall/parent-ctx), keep only wins. Baseline
finding (2026-07-20): across 240+ sessions the agent has **never** spawned a
subagent, and `fanout-bugs` is solved serially/cheaply (0/3 delegated) — a small
fan-out doesn't pressure delegation. Making delegation *win* needs heavier,
genuinely-independent subtasks (so serial bloats the parent) — that's the next
task to author before nudging the prompt (per "Do More Agents Help?", nudging
delegation on tasks that don't need it usually hurts).

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
