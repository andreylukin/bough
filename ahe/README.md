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
`patch-grammar.md`, `lsp.md` — which is the half of the paper's "tools" component
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
