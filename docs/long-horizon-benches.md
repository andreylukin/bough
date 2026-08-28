# Benchmarks for tuning long-term building

Andrey, 2026-08-28: "see if there are other benchmarks I can use to tune long term building."
Companion to `docs/deep-swe.md`. Surveyed with Exa on 2026-08-28; every claim below is from the
benchmark's own site, paper or repo.

The projection is what carries a build past one context window: tiers, pins, digest, the tail.
A benchmark is useful for tuning it only if (1) the horizon is long enough that the projection
degrades at all, (2) the reward is a verifier, not a judge, and (3) bough can be plugged in
without forking the runner. Column three decides the order of work: everything in the **Harbor
task format** runs through the one Pier/Harbor adapter `docs/deep-swe.md` specifies.

## Tier 1 — Harbor format, real horizon, verifiers. One adapter runs all of them.

| bench | what | horizon | reward | fit |
|---|---|---|---|---|
| **DeepSWE** (datacurve, 113 tasks, 5 languages) | original feature tasks in active repos | 3 h cap; solutions 5.5× SWE-Bench Pro's | hand-written functional verifier | primary — `docs/deep-swe.md` |
| **SWE-Marathon** v1.1 (abundant-ai, 20 tasks) | build a C compiler in Rust, reimplement Kubernetes, port BioFabric, rebuild Next.js on Vite, product clones | ultra-long: 3–8 h and **25–250 M tokens per attempt** (their `long-horizon` repo's own estimates) | binary (every visible + hidden test) plus an uncalibrated partial score; CUA rubric for clones; k = 8 | the compaction STRESS test — one or two tasks, not a bank; Opus 5 tops at 50 % pass@1 |
| **Terminal-Bench 2.x / 3** (Harbor Hub) | 89+ terminal tasks, continuous releases | median well under an hour | tests | regression bank, not a horizon bench; bough already scored on it |
| Harbor Hub: `swebenchpro`, `swelancer`, `swesmith` | issue-fixing and freelance tasks | short–medium | inherited PR tests | comparability only; inherited tests are what DeepSWE was built to escape |

## Tier 2 — build from scratch or maintain over time; own runners, thin adapters

| bench | what | horizon | reward | fit |
|---|---|---|---|---|
| **StaminaBench** (AWS, 2026-06) | implement a REST server, then **100 procedurally generated change requests** on the same codebase (up to 6 k LoC); tests generated without an LLM | 100 turns; every model fails within 5–6 turns without test feedback, up to 12× more with it; a 6× gap between best and worst harness for the same model | pass fraction per turn, averaged | **the continuity bench**: one trajectory, one ledger, a hundred wakes — exactly what tiers and pins are for; the harness is a thin Python wrapper around a CLI over stdin/stdout in Docker, so `bough exec` on a persistent home fits; cheap with haiku |
| **NL2Repo-Bench** (ByteDance Seed, ICML 2026; 103 Python libraries) | a full installable library from one requirements doc and an empty workspace; ~90 tool calls | medium | the original pytest suite, **continuous** | "building" proper; OpenReward environment + per-task Docker images on GHCR; needs a small adapter |
| **Commit0** (ICLR 2025; 57 Python libraries) | rebuild a library from spec + docs; lint and type checks | medium | unit tests, continuous | older cousin of NL2Repo; interactive env, `pip install commit0` |
| **RepoZero** (Baidu/PKU, 2026-05) | reproduce a repository from its API spec, cross-language, black-box output equivalence | medium | execution equivalence | strongest anti-leak design; runner unverified |
| **SWE-CI** (2026-03; 100 tasks) | maintain a codebase through its real evolution: avg **233 days, 71 consecutive commits** per task, multiple rounds | long, multi-round | CI tests per round | long-term maintainability — the ledger's reconsolidation and expiry are what this measures; HF dataset, runner in repo |
| **LongCLI-Bench** (ACL 2026 Findings; 20 tasks) | from-scratch, feature, bugfix, refactor; 104 files and 15 k LoC per task, 1000+ expert minutes | long | fail→pass and pass→pass with step-level scoring; best agents < 20 % | good diagnostics (which step failed); own runner |

## Tier 3 — context management directly, not software building

| bench | what | fit |
|---|---|---|
| **LOCA-bench** (HKUST, 2026-02) | agent tasks whose context can be grown **to arbitrary size with fixed semantics**; evaluates model + scaffold + context strategy; has a Claude Code path | the cleanest ablation of the degradation ladder (`budget_tokens`, `headroom`, tiers): same task, growing context, does the projection hold |
| **agent-context-benchmark** (2026-08) | Claude Code vs Codex on six context-lifecycle tests: instruction scope, skill loading, MCP choice, **session resume, compaction recall, handoff** | small, product-level, but the compaction-recall and resume tests are the projection's job description; borrow the tests |
| **The Compaction Cliff** (Passau, 2026-08) | Claude Code's `/compact` keeps 53 % of safety rules after one round, 10 % after five | a **pins invariant**: a standing rule must survive N seals verbatim. Cheap to run locally against `rollups`; add as a gate |
| **AMA-Bench** (UCSD) | long-horizon memory QA (recall, causal, state-updating, abstraction) over agent trajectories, incl. SWE-bench | memory QA, not building; the four capability names are a good rubric for reading a digest |
| **Context as a Tool** (ACL 2026), **SWE-MeM** (2026-06), **Scroll / Context as an Environment** (Alibaba, 2026-08) | methods, not benches: callable compaction, memory-aware RL, an event log + kernel with evictable spans and landmarks | Scroll's "append-only event log, only printed projections enter the view, evicted spans recoverable by address" is bough's ledger + projection restated; worth reading for the eviction index |

Not a fit: SWE-Bench Pro (short, inherited tests, contamination — the reasons DeepSWE exists),
ProdCodeBench (Meta-internal), GitTaskBench (tool-use over repos), METR's time-horizon suite
(mostly private; their note: Claude Code and Codex do **not** beat their plain ReAct/Triframe
scaffolds on time horizon — a harness's value shows up elsewhere than on their tasks).

## What to run, in order

1. **DeepSWE** — the plan already written; the adapter is also the adapter for 2 and 4.
2. **StaminaBench** at 100 turns on one persistent `$BOUGH_HOME` — the direct read on whether the
   tiers and pins keep a build consistent across a hundred wakes. Reward per turn is the curve
   the arms are compared on. Cheap.
3. **The Compaction Cliff test as a pins gate** — N standing rules, K seal passes, count what
   survives verbatim in the assembled projection. Runs in `make bench` with no API beyond the
   summarizer.
4. **NL2Repo-Bench** subset (10 libraries) for building from nothing — continuous reward makes
   arms comparable with small n.
5. **SWE-Marathon**: one task (the Java LSP or Kubernetes) as the once-a-week compaction stress
   test; read the recorded contexts, not the score.
6. **LOCA-bench** when the degradation ladder itself is being redesigned.
