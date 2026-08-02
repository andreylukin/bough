# ahe — observability-driven prompt evolution for bough

An implementation of [Agentic Harness Engineering](https://arxiv.org/abs/2604.25850)
(arXiv 2604.25850) against bough, narrowed to one editable surface and stripped of
every hosted dependency.

`evaluate → analyze → improve`, with the base model frozen and only the harness
edited, so a pass-rate change has one candidate cause.

## What is different from the paper

**No hosted infrastructure.** The reference implementation runs every rollout in an
E2B sandbox (SaaS or a self-hosted cluster), spawned from prebuilt templates, over
harbor datasets, with NexAU as the harness substrate and Serper and Langfuse
alongside. None of that is load-bearing for a local loop: rollouts run in temp
directories against an isolated bough server on its own port, home and database,
and the task bank is four files in a directory. Their Agent Debugger is only
partially open-sourced anyway, so the analysis layer here is a reimplementation
either way.

**One editable component, not seven.** The paper evolves the system prompt, tool
descriptions, tool implementations, middleware, skills, sub-agent configs and
long-term memory. This loop edits `src/prompt/*.md` and nothing else.

That is a real narrowing of the method and it cuts against the paper's own
ablation, which found the system prompt to be the one component that **regressed**
alone while tools, middleware and memory each carried the gain. The mitigating fact
is what bough's prompt actually is: most of those files are not strategy prose but
the interface documentation for the host functions — `shell.md`, `files.md`,
`patch-grammar.md`, `searching.md` — which is the half of the paper's "tools" component
that transferred. The evolve agent is pushed toward those and away from
exhortation, but the risk is real and the loop should be judged on measured flips,
not on the fact that it ran.

## The three observability pillars, as implemented

**Component observability.** `src/prompt/assemble.ts` already represents the prompt
as one markdown file per section, deliberately with no inlined TypeScript copy, and
includes each section conditionally on the capability it documents. So the action
space is a directory listing, and a rollback is `git checkout` of one file.

**Experience observability.** `materialize.ts` explodes each trial into a directory
an agent navigates rather than a payload it swallows: `README.md` to start,
`rounds/round-NNN.md` per round, the raw JSON beside it, and `hostfn_events.jsonl`
pairing every host-function call with the result that arrives a round later. Because
bough is code-mode, that pairing has to be derived — the model writes a program, so
there is no tool call per verb to count — and it is what maps a failure to a single
prompt section.

**Decision observability.** Every edit ships a `change_manifest.json` entry with
failure evidence, root cause, targeted fix, and a prediction of which tasks flip.
The next iteration settles it against the flip matrix and reverts the file if it
did not hold.

## Running it

    bun ahe/sweep.ts -n3                  # evaluate the bank, k=3
    bun ahe/sweep.ts -n3 bugfix-precedence
    bun ahe/loop.ts 5                     # five full iterations

A sweep restarts the bench server by default. It has to: prompt sections are read
and memoized at first use, so a server left over from a previous sweep serves the
pre-edit prompt while the results file records the post-edit sha — an invalidated
measurement with no visible symptom.

## Adding a task

    ahe/tasks/<name>/
      prompt.md      the request, as a user would put it
      fixture/       the starting workspace, copied fresh per trial
      hidden/        the grading suite — never in the workspace
      verify.sh      reads the workspace, exits 0 or prints one FAIL line
      reference/     a known-good solution, for gating the verifier

Four rules, each of which some earlier bench violated:

1. **Outcome-graded.** `verify.sh` inspects the code, never the transcript. An agent
   that reports success on broken code and one that reports failure on working code
   land on opposite sides of that line.
2. **Mutation-gated both ways.** The verifier must accept `reference/` and reject
   the pristine fixture *and* each plausible partial fix. A verifier that passes
   everything is worse than no task.
3. **Variance-labeled.** Run k=6 before admitting it. A task between roughly 20% and
   80% is a coin flip — keep it, but record the baseline so the loop does not chase
   its noise as a signal.
4. **Exercises host functions deliberately.** A section no task exercises is a
   section the loop can never improve, and the manifests make that visible.

## What the first real iteration did

Recorded end to end under `ahe/runs/`, on one task at k=4:

1. The sweep scored 4/4 — nothing to learn from flips.
2. The analyzer read the traces anyway and found that in three of four *passing*
   trials the agent re-bound a host-function name (`const { bash } = globalThis`,
   an import from a package that does not exist, a same-named helper) and the
   program failed pre-flight before it ran. It cited the rounds.
3. Its root cause: `identity.md` stated the rule with one example, the plain
   `const bash = ...` form, and none of the three real failures took that shape.
4. The evolve agent edited that sentence to name the shapes that occurred, and
   predicted parse errors would fall with no task flipping. +180 chars, replacing
   rather than appending.
5. The next sweep: parse errors 3 → 0, exactly as predicted. And every trial ran
   more rounds — 18→30, 26→33, 22→26, 22→39. The edit was **reverted**.

That last step is the paper's reported blind spot, reproduced on iteration one,
and it is why the waste path carries a round-count guard. The analysis also caught
a real bug in this harness's own instrument: one failed program was being counted
as several failures, once per host function it named.

## The second iteration — a discriminating task

`perf-overlap` (baseline 0/4) put the loop on a task with real headroom.

The analyzer's finding, reached independently of my own reading of the same
traces: every trial *did* benchmark its work — and every one built the benchmark
at a size it chose itself (10k-50k events, a handful of windows) rather than the
200,000 events and 1,000 queries the request named. At that scale a prefix-bounded
scan looks fast, so the measurement confirmed the complexity story the agent had
already told itself. Trial 1 closed with "Performance test on 50k events: ~2.1ms
per query" and failed the budget by 3.6x.

Its edit added one paragraph to `ending.md`: a speed claim counts as verified only
if measured at the size the request named. Targeted at the step that failed,
not exhortation. Predicted: `perf-overlap` flips.

Settled at k=4: **0/4 → 0/4. Refuted, reverted.** With the edit in place, one trial
benchmarked at 100k and the rest stayed at 1,000; the stated 200,000 never appeared
in any trial in either sweep. The waste metrics did improve (parse errors 3 → 0,
host-function errors 6 → 1, rounds 144 → 129, cost $0.565 → $0.489) — but the edit
predicted a flip, not that, and `settle()` refuses to credit an unpredicted
improvement. At k=4 those deltas are not evidence anyway.

Which is the point of the apparatus. A well-argued, evidence-backed, correctly
diagnosed prompt edit did nothing to the outcome it named, and the loop said so
and removed it, in the same shape the paper's ablation predicts for system-prompt
edits.

## Reusing the Terminal-Bench bank

`ahe/harbor/bough_agent.py` registers bough as a Harbor agent, so the ~100
Apache-2.0 Terminal-Bench tasks can be run without writing them. Harbor owns the
container, the network policy, the verifier and the reward; we own getting bough
into the image and running one turn. Every task ships an oracle solution, which
is the expensive half of admitting a task to a bank.

    cd ~/hb && PYTHONPATH=$HOME/hb harbor run -d terminal-bench@2.0 -l 5 \
      -a bough_agent:Bough -m claude-haiku-4-5

Verified 2026-07-30: reward 1.0 on a smoke task, and a clean run on the real TB2
task `gpt2-codegolf` — 0 exceptions, reward 0.0, with the per-turn trace and
prompt-section manifest recovered from inside the container. The evidence layer
works in someone else's sandbox.

Two things to weigh before adopting it as *the* bank:

**Floor effects are as bad as ceiling effects.** TB2 is calibrated so that
frontier models sit near 70%; haiku sits near the floor. `gpt2-codegolf` is not a
harness signal, it is a task haiku was never going to do. A bank where everything
fails teaches this loop exactly as little as one where everything passes, so the
per-task haiku band has to be measured and the middle kept — a sweep of its own
before any evolution starts. TB-Core v0.1.1 is the older, easier set.

**The task mix points away from the action space.** Terminal-Bench is terminal
and sysadmin shaped, which exercises `bash()` heavily and `patch()` and `view()`
barely. Since the editable surface here is the prompt, and the part of the
prompt most likely to transfer is the host-function contracts, a bank that never
exercises those contracts cannot produce evidence about them. Use TB as a source
for calibrated difficulty, and keep hand-written tasks for the verbs it misses.

## Three prompt edits, three refutations

Run on gpt-5.6-luna (~20x cheaper per trial than haiku, same discriminating
shape), the loop has now produced three well-argued, evidence-backed, correctly
diagnosed edits and settled all three as refuted:

| iteration | edit | prediction | outcome |
|---|---|---|---|
| 1 | `identity.md` — name the shapes that shadow a host function | parse errors fall | fell 3 -> 0, but every trial ran ~45% more rounds. Reverted. |
| 2 | `ending.md` — verify a speed claim at the size the request named | perf-overlap flips | 0/4 -> 0/4. Reverted. |
| 3 | `files.md` — the `+` body rows DO travel through the JS literal | parse errors fall | rose 9 -> 12. Reverted. |

The third is the one that should have worked. `files.md` did not merely omit the
rule — it stated the opposite: "backticks and `${...}` in the target file cannot
corrupt the match" is true of matched lines, which are named by line number and
never quoted, and false of `+` body rows, which are new text travelling through
the JS literal. The prompt was telling the model that the failure it kept hitting
could not happen. Correcting a false contract statement is the most promising
edit class there is, and it still did not hold.

That is now consistent evidence rather than a suspicion, and it matches the
paper's ablation exactly: the system prompt is the component that does not carry.
Both defects the loop found — host-function name shadowing, and file content
breaking the JS literal it is embedded in — have structural fixes outside the
prompt, and the loop keeps correctly identifying causes it is not allowed to fix.

**Do not change the bank mid-experiment.** `patch-refactor` was added between
sweeps 1 and 2, so the summary totals compared 8 trials against 12, and `settle()`
would have read a new task's numbers as an effect of the edit. Settlements are
computed over the tasks present in both sweeps. This is the same class of silent
invalidation as a stale server.

## Known limits

**Bank size.** The paper runs 89 Terminal-Bench 2 tasks. Below roughly 40 a single
flip is several points of pass rate and the loop will attribute noise with
confidence. Growing the bank is the prerequisite for trusting any verdict this
produces.

**Regression blindness.** AHE reports that self-attribution is reliable for fixes
and blind to regressions — an edit that fixes one task and quietly breaks another
gets credited for the fix. `settle()` requires no net regression as a mitigation,
which is not a solution.

**Model mismatch.** The bank runs on haiku for cost. Optimal harnesses are
model-specific, which is the paper's own point, so an edit confirmed here is an
edit confirmed for haiku — carrying it to the model you actually drive is a second
claim needing its own evidence.
