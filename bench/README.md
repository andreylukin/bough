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
pool). Morning after: `bench/tune/report.py`, then adopt a winner with
`bench/tune/adopt.sh <variant>`, which copies its section files into the
checked-in default prompt dir `src/supervisor/prompt/` — where
`promptOverride()` reads them in normal operation, so adoption is a reviewable
`.md` diff, not a TS-array edit. `variants/baseline/` and `src/supervisor/prompt/`
are both dumped from the built-in TS arrays by `tune/dump-prompt.ts`; reseed
after any change to the builtins in `prompt.ts`.

**No restart per variant.** A prompt variant is pinned per session via
`bough exec --prompt-dir` (→ `session.promptDir` → the turn runner resolves the
sections for that session), so one long-lived bench server serves every variant.
`run-bough.sh` passes `--prompt-dir "$BOUGH_PROMPT_DIR"`; `run.sh` honors
`BENCH_KEEP_SERVER=1` (reuse a running server); the tuner starts one fresh server
per campaign and keeps it. Manual `run.sh` still restarts by default (stale-code
guard). This is what makes the loop fast enough to run continuously.

## Continuous pipeline (bench/tune/)

The tuner runs as a standing loop, not a one-off. Two halves feed the same
haiku-bench adoption gate — nothing ships without beating baseline at n=6.

- **Offline (nightly):** `bench/tune/nightly.sh` runs one campaign and, on a
  *confirmed* champion (machine-readable in `results/tune-<campaign>-summary.json`),
  opens an adoption PR — built in a throwaway `git worktree` off `origin/main`
  (never the dirty dev checkout), `adopt.sh`'d, pushed, `gh pr create`'d. A human
  merge is the actual prompt change. Schedule it with
  `com.bough.prompt-tune.plist` (launchd, 02:30; `launchctl load` it). Knobs:
  `HOURS`/`TRIALS`/`TASKS`/`PROPOSER_MODEL`, `NO_PR=1` to build the branch without
  pushing.
- **Online (ACE):** `bench/tune/online.py` mines real daily-driver sessions
  (`~/.bough/bough.db`) for recurring friction — failed checks, tool-error bursts,
  rework loops — and a `claude -p` reflector (contract in `tune/online.md`) turns
  patterns into candidate prompt deltas (ACE, arXiv 2510.04618: generator →
  reflector → curator, incremental deltas, no monolithic rewrite). Candidates are
  materialized under `variants/` tagged `source=online`, growth-capped, and queued
  (`variants/_online-queue.json`). They are only HYPOTHESES: `tune.py --seed-online`
  (which `nightly.sh` passes) races queued candidates as the first challengers.
  Real sessions run on a stronger model than the bench, so a delta that doesn't
  move the weak-model bench is dropped — the friction is the idea source, the
  bench is the judge. `--dry-run` prints the friction summary without an LLM call.

Not yet in the bank (deferred, honest gaps): a *real-repo* bugfix task (needs a
vendored fixture repo) and a *tool-use/MCP* task (needs an MCP server wired into
both headless runners). Add tasks the same way — `fixture/` + `prompt.md` +
`verify.sh` grading final workspace state; keep the `decomposableRequest`
calibration in `prompt.test.ts` honest by prefixing any genuinely fan-out task
`fanout-`.

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
| fanout-eight | 8 INDEPENDENT modules (~230 lines), each with a distinct non-obvious bug (boundary, index swap, leap-year, BFS-vs-DFS, off-by-one, sample-vs-population divisor) | all eight suites green + tests untouched — the heaviest fan-out, by design pushed past where a weak model's later fixes degrade under accumulated context, so delegation should start to move *pass rate*, not just parent-ctx |
| feature-topwords | add `--top N` flag to a CLI | exact output incl. tie-break, old behavior intact |
| longhorizon-feature | multi-step CLI extension: `-p/--priority` flag + data-model change + `done`/`top` commands + priority sort with tie-breaks + two error paths | scripted session checks exact stdout, sort order, tie-breaks, and error exit codes — the long-horizon planning shape |
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

That is the *quantitative* trajectory view. For the *qualitative* one — was the
solve clean or a lucky flail — `python3 bench/rate.py <since_ts> [task]` reads each
trial's transcript and has a strong judge model (`RATE_MODEL`) score it 1..5 on
approach / efficiency / verification / recovery, with the oracle verdict given as
ground truth so a pass-by-luck and a clean solve diverge; it flags the
**verification gap** (claimed success the oracle failed, or a pass with no check
run) and appends `results/ratings.jsonl`.

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
