# Worker redesign — decoupling the supervisor–worker loop

Status: **plan** (no engine changes yet). Companion to `SPEC.md §5`.
Goal (per owner): optimize for **all three** — cost, latency, reliability.

The benchmark harness has been **restored** to
`packages/bough_server/src/bough_server/experiment.gleam` (recovered from
`ecd0bc6^`, compiles clean against the current tree) so the proposals below are
testable. Run modes: `BOUGH_EXP_MODE=worker|relay|escalate|line|supervisor`,
suites `BOUGH_EXP_SUITE=easy|hard`.

---

## 1. Current state

### Code reality
- **Supervisor** (frontier, default Haiku): one `run_steps` call/turn; primary
  action `code` = a monty Python program calling `bash/read/write/edit`; commits
  a `### CHECK` and `done`. `engine.gleam` drives the round loop.
- **Harness** (deterministic): `run_check` (engine.gleam:1103) re-runs the CHECK
  each round; `handle_done` (engine.gleam:630) gates on `check_ok && reviewed`.
- **Worker** (local qwen2.5-coder-3b): wired **only** at `apply_with_fixes →
  fix_loop` (engine.gleam:730–795). On a non-zero step exit it gets **one** shot
  (`fix_attempts: 1`) at a **single shell command** (`prompts.worker_system`),
  **synchronously, on the supervisor's critical path**.
- **Subagents** (`spawn/tell/collect`): a *separate, async* delegation path via
  `state.wake` — full nested agents, not the local worker.

### What the history shows (`~/.bough/sessions`)
| step type | count |
|---|---|
| exec / call (supervisor) | ~2,448 |
| plan | 1,305 |
| check | 265 |
| review | 122 |
| **worker** | **22** |
| spawn/subagent | ~0 |

The worker fires **<1%** of the time and is mostly counterproductive: its top
trigger is the supervisor writing `import os` in a `code` step → monty (a Python
*subset*) rejects it → the worker, told "propose one shell command," answers
**`pip install os`** (exit 127). It has no model of the failure class. In
practice bough is a **supervisor-only** agent; the worker is vestigial.

### SPEC tension to reconcile
`SPEC §5.2` claims the harness is "built around" a *small* code-strong model
writing the code-mode program. The experiments (memory: `worker-model-usage`)
proved the opposite — small models **collapse on monty code-mode** (the
indirection wall: they redefine `read/write` via `os`/`open`, or emit the answer
without ever calling `write()`), and only work on the **primitives** surface
(block body == file content). So today the *frontier* supervisor writes
code-mode and the small worker can't. The redesign accepts this: **frontier on
code-mode, small worker on primitives.**

---

## 2. Research basis

- **ReDAct — Uncertainty-Aware Deferral for LLM Agents** (arxiv 2604.07036): a
  small cheap model acts **by default**; defer to a large model only when
  predictive uncertainty crosses a calibrated threshold. ~15% deferral matches
  full large-model quality at a fraction of cost. bough is currently the
  **inverse**. bough's advantage over ReDAct's UQ gate: a deterministic **CHECK**
  is a *stronger* deferral signal than uncertainty whenever a check exists.
- **RP-ReAct** (arxiv 2512.03560): decouple a strong Reasoner-Planner from a
  cheap executor; offload large tool outputs to external storage. Reports the
  monolithic plan-execute loop "causes trajectory instability"; decoupling
  improves stability *across model scales*. bough already has the external-storage
  half (the blackboard digest); it lacks the decoupled-executor half.
- **Anthropic — Building Effective Agents / Multi-agent research**: simplest
  pattern that works; orchestrator-workers earn their ~15× token cost only for
  unpredictable, parallelizable, read-heavy subtasks — not tight-coordination
  coding.
- **Prior bough experiments** (memory): `escalate` mode — tier1 qwen one-shot →
  ≤1 local relay (VibeThinker-3B) → haiku backstop, CHECK-gated — hit 100%
  solved, p50 ~760ms, p95 13s. Executor floor = qwen2.5-coder:**3b** (1.5b
  collapses on the hard suite). Instruction specificity (S1: file+fn+behavior+
  example) materially raises one-shot success.

---

## 3. Diagnosis — the loop is tight in the wrong way

1. **Synchronous & inline.** `fix_loop` blocks the round on local inference
   (relay measured at 7–57s). Subagents are async; the worker isn't.
2. **Sub-atomic unit.** It patches one failed step with one shell command — it
   can never own a verifiable unit and iterate-to-green locally, so the
   supervisor must re-take the wheel every round.
3. **No verification of its own.** The CHECK lives only at the supervisor's round
   boundary; the worker can't self-gate or self-escalate.
4. **Failure-class blind.** No monty/sandbox awareness → `pip install os`.

---

## 4. Target architecture

Reframe the worker from **fixer** → **executor of a delegated, verifiable
unit**, with escalation (ReDAct deferral) as the mechanism and the CHECK as the
gate.

```
supervisor (frontier, code-mode)
  └─ delegate(unit)  ──async via wake──►  worker tier ladder:
        tier1  qwen2.5-coder-3b  one-shot  (primitives surface)
        tier2  VibeThinker-3B relay        ≤1 round
        tier3  frontier/haiku backstop  (or fold into spawn)
     gate at every tier = the unit's local CHECK
     result delivered on wake (supervisor turn never blocks)
```

A **unit** = `{location: file+fn, target_behavior + example, local_check}` —
memory's S1 specificity rung (dictate *where* + *what-outcome*, not *how*).

---

## 5. Implementation plan (phased; file-level)

> **Status (2026-06-29):** Phase D shipped (prompt). **Phases A + B implemented**
> on branch `worker-delegation-ladder`: a `delegate` action runs the worker
> best-of-N (`delegate_samples`, default 2) against a per-unit `check`, on the
> write/edit/sh primitives surface, with `worker_max_tokens` capping the worker's
> CoT — the two measured wins, now real `engine.Config` fields. tier3 = the
> supervisor itself (it gets the failure and takes over); the VibeThinker relay
> (tier2) is deferred. Build + 31 tests green; parser + wiring unit-tested; the
> best-of-N+CHECK algorithm is the harness-validated one (93%). Phase C (async)
> still open.

**Phase D (cheap win, do first, low risk) — fix the failure-class mismatch.**
- `prompts.worker_system`: add monty-subset awareness (the failure is a
  sandbox/semantics issue, not a missing package → never install anything, never
  `import os`) and keep it **shell-only**.
- **REVISED after testing (2026-06-29):** do NOT add a `write`/`edit` affordance
  here. Tested against the real historical failure on qwen2.5-coder-3b
  (llama-server:8080): the monty-aware **sh-only** prompt produced the correct
  `find … -type d` on an exploration failure; but adding a `write` option — even
  *guarded* with "use sh for commands, write only for file contents" — made the
  3B **over-select write and hallucinate** (it invented a fake README, emitted
  `pip install -r requirements.txt` inside it). The small worker cannot
  self-select the channel. Write only fires correctly when the brief is
  unambiguously a file-contents bug. → **Phase D = sh-only + monty-aware**
  (clean, validated); the write/edit channel moves to Phase A where the
  *supervisor* dictates it.
- `engine.fix_loop`: leave the shell path as-is (`artifact.first_fence` →
  `exec_run`). No new parsing needed for Phase D.

**Phase A — delegation primitive.**
- New action in the `run_steps` schema (`tools.gleam`) + `artifact`/`Step`
  (`bough_core/artifact.gleam`, `agent.gleam Step`): `delegate` carrying
  `location`, `target_behavior`, `example`, `check`.
- Engine handler: run the worker on the **primitives** surface against the unit,
  gated by the unit's local check. **The supervisor dictates the channel**
  (write/edit for a file fix, sh for a command) — never the 3B, which can't
  choose reliably (Phase D test). This is where `write`/`edit` parsing lands.

**Phase B — CHECK-gated tier ladder** (replaces fixed `fix_attempts`).
- tier1 qwen one-shot → tier2 ≤1 VibeThinker relay → tier3 frontier backstop.
- Gate = the unit's local check (deterministic). Add a soft confidence/logprob
  gate (ReDAct) only for units with no committable check yet.
- Config: `engine.Config` already has `worker_temperature/top_p`; add per-tier
  models + caps. Stack: executor=qwen2.5-coder:3b, reasoner=VibeThinker-3B,
  backstop=haiku.

**Phase C — async (the literal "less tight loop").**
- Route delegated units through the same `state.wake` path subagents use, so the
  supervisor's turn doesn't block on local latency; deliver result on wake.
- Consider unifying tier3 with `spawn` (one delegation ladder, not two).

**Spec.** Update `SPEC §5.1/§5.2/§5.6` to match: frontier on code-mode, worker
on primitives, deferral ladder, async delegation.

---

## 6. Harness adaptation (experiment.gleam)

Restored as-is it already validates the tier ladder (`escalate_sweep`,
`relay_sweep`) and instruction specificity (`line_sweep`) on the QA'd easy/hard
suites with an **independent grader** (does not trust the model's own check).
Adaptations needed for the new design:
- Add a `delegate`-primitive mode that exercises Phase A/B through the real
  engine path (today `escalate_sweep` calls the worker directly).
- Add metrics the old harness lacks (see §7).
- Keep the QA invariant for any new task: fixture must FAIL the check; a correct
  solution must PASS.

---

## 7. Test matrix

| # | Test | Pass bar |
|---|---|---|
| 1 | Regression: easy+hard suites, `escalate` | ≥ memory baseline: 100% solved, p50 <1s, p95 <13s, hard tier1 ≥90% |
| 2 | Phase D alone: replay worker-trigger sessions (`mSmNe6k70qM6aL1P` ×12, `TBSNG76` ×4) old-path vs new | zero `pip install os`-class responses; ≥ as many fixes land |
| 3 | Deferral rate (ReDAct metric) | ~15% of units reach frontier; rest solved locally |
| 4 | Latency: supervisor wall-clock **blocked** vs async | async ≈ 0 blocked on worker inference |
| 5 | Cost: tokens/task vs supervisor-only baseline | ≤ baseline (worker absorbs local work) |
| 6 | Supervisor rounds/task | drops vs today (worker owns inner loop) |
| 7 | Ablation: primitives vs monty code-mode for executor | code-mode collapses (confirms §1 premise) |
| 8 | Ablation: tier2 on/off | quantify tier2's marginal solves vs latency (memory: only 6/45) |

Invocation: `gleam run -m bough_server/experiment` with the `BOUGH_EXP_*` env
(worker endpoint, models, suite, trials). Needs the local worker/llama-server +
GGUFs running; `ANTHROPIC_API_KEY` for the frontier backstop and baseline.

### Baseline captured 2026-06-29 (`escalate`, 1 trial)
Stack: executor=qwen2.5-coder:3b, reasoner=VibeThinker-3B-GGUF, backstop=
claude-haiku-4-5 — all via ollama (11434), real engine path for tier3.

| suite | solved | tier1 (fast) | tier2 | tier3 | p50 | p95 / max |
|---|---|---|---|---|---|---|
| easy (15) | 15/15 (100%) | 11 (73%) | 1 | 3 | 733ms | **149.8s** |
| hard (10) | 10/10 (100%) | 9 (90%) | 1 | 0 | 1.9s | 22.4s |
| combined (25) | 25/25 (100%) | 20 (80%) | 2 | 3 | — | — |

Matches memory's prior numbers on solve-rate (100%), easy p50 (~760ms), and the
hard-suite tier1 win for qwen-3b (90%, p50 ~1.7s). **Regression vs memory: the
easy-suite tail blew out to p95/max 149.8s** (all on `t5_brackets`). Cause = the
reasoner's unbounded CoT on hard local tasks — exactly the "cap reasoner
thinking" lever in §8.

### A/B: reasoner CoT cap (3 trials, uncapped vs `BOUGH_EXP_REASONER_MAXTOK=1500`)
The single-trial tail understated it; at 3 trials uncapped p95 was 41s (easy) /
124s (hard). Capping the reasoner's token budget cut the tail **3–4× with zero
solve-rate loss** — slow local relays defer to the haiku backstop fast instead
of grinding a long CoT (the ReDAct tradeoff, made cheap):

| metric | easy uncapped → capped | hard uncapped → capped |
|---|---|---|
| solved | 45/45 → 45/45 | 30/30 → 30/30 |
| tier1 | 35 → 38 | 21 → 27 |
| p50 | 833 → 700 ms | 1931 → 1787 ms |
| **p95** | **41.4s → 11.7s** | **123.8s → 30.6s** |
| **max** | **159.3s → 43.3s** | 150.3s → 50.6s |

The cap is now an env knob (`experiment.gleam` relay_loop); promote it to a real
config (`engine.Config.worker_max_tokens` per tier) in Phase B. **Takeaway: the
ladder's correctness is solid (100%); the latency tail — the daily-driving pain
— is fixed cheaply by a CoT cap, and further by Phase C (async).**

### Executor size A/B (hard suite, 2 trials, reasoner capped 1500) — 2026-06-29
Question: should the worker be a larger SLM? **No — strictly dominated.** Latency
rises monotonically with size while fast-path hit-rate does not improve:

| executor | tier1 hit | p50 | p95 | max | solved |
|---|---|---|---|---|---|
| qwen2.5-coder:**3b** | **17/20 (85%)** | **1.8s** | 51s | 52s | 20/20 |
| qwen2.5-coder:7b | 14/20 (70%) | 3.5s | 55s | 60s | 20/20 |
| qwen2.5-coder:14b | 16/20 (80%) | 6.4s | 52s | 75s | 20/20 |
| qwen3-coder:30b (memory) | ~25% | ~23s | — | — | — |

All reach 100% final solve (the ladder backstops). The worker tier's job is a
sub-second one-shot; capability beyond one shot is what the CHECK gate defers to
the reasoner→haiku tiers. Levers that *do* raise the fast path: format-compliance
(lenient parser), S1 instruction specificity, reasoner cap — not worker size.
Keep the executor floor at **3b**.

### 3b fast-path levers — tested 2026-06-29 (hard suite, 3 trials, capped reasoner)
Two candidates for raising the 3b's tier1 hit-rate without a bigger model:

- **Lenient parser (format compliance)** — `BOUGH_EXP_LENIENT`, now reverted.
  Hypothesis: accept a tagless ```python full-file block as a write (the miss
  that dropped the 30b's correct fixes). **Negative result for the 3b:** tier1
  25→**did not improve**, and it slightly regressed (h_grades/h_shop 3→2) by
  misfiring on non-full-file blocks. The 3b already emits ```write correctly
  (~90% tier1) — no format-misses to recover. The lever was a 30b problem; it
  does not transfer. Reverted (no speculative code).
- **Tier1 best-of-N (variance harvest)** — `BOUGH_EXP_TIER1_SAMPLES`, KEPT.
  The 3b has high per-shot variance; a second ~sub-second sample is ~100× cheaper
  than escalating. **Clear win:**

  | metric | best-of-1 | best-of-2 |
  |---|---|---|
  | tier1 hit | 25/30 (83%) | **28/30 (93%)** |
  | tier3 escalations | 5 | **1** |
  | p95 | 46.5s | **12.9s** |
  | p50 | 1778ms | 1779ms (unchanged) |
  | solved | 30/30 | 30/30 |

  A second sample on the ~17% that miss converts 4/5 frontier escalations into
  cheap local solves — tail **3.6×** lower, **zero p50 cost** (first-try passes
  never retry). Remaining ceiling = `h_eval` (genuine capability → frontier).

**Two proven, cheap levers to promote into `engine.Config` in Phase B:**
reasoner CoT cap + tier1 best-of-2. Both cut the tail with no solve-rate or
common-case cost. Worker size does NOT help (see size A/B above).

### Phase D shipped (prompt-only, validated 2026-06-29)
`prompts.worker_system` is now monty-aware + shell-only (engine.gleam reverted to
zero diff). Validated against the exact historical failure on qwen2.5-coder-3b:
the `import os` step now yields `find … -type d` instead of `pip install os`.
The write/edit affordance was tested and **rejected for the fix path** (the 3B
over-selects write and hallucinates file contents) — it moves to Phase A.

---

## 8. Risks / open questions
- **Latency of local inference** is the core daily-driving pain; Phase C (async)
  is what makes the worker usable at all. MLX runtime (2–3× on Apple Silicon)
  and capped reasoner thinking are follow-on levers.
- **tier2 may not earn its keep** (memory: 6/45 solves); a tier1→tier3 ladder is
  a valid simpler fallback. Decide via test #8.
- **Unifying tier3 with `spawn`** vs keeping them separate — affects whether
  there's one delegation ladder or two.
- **Confidence gate for un-checkable units** — only needed if we delegate work
  that can't carry a local check; may be deferrable.
