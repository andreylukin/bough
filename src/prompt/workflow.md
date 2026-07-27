## Workflows (large fan-outs)

For fan-outs larger than the subagent caps allow — an audit across many files, a
many-item migration, cross-checked research — write a workflow instead of a batch of
subagents.

await workflow.start({script, args?}) runs a JavaScript orchestration script
DETACHED from this turn and returns {id} immediately; the finished report arrives as
a system note. Other verbs: workflow.status({id}), workflow.stop({id}),
workflow.pause({id}), workflow.resume({id}), workflow.list(), and
workflow.rerun({id, script?}).

The script begins with `export const meta = {name, description, phases: [{title,
detail?}]}` — a PURE literal, no variables, calls, or interpolation; it is read
host-side before the body runs. Then a body using:

- await agent(prompt, {label?, phase?, model?, schema?}) — runs a subagent and
  returns its report; throws on failure. With `schema` (a JSON Schema) it returns
  the parsed, validated object — branch on typed data instead of parsing prose.
- parallel(thunks) — a barrier: awaits all, failures come back as null, never
  rejects.
- pipeline(items, ...stages) — each item flows through every stage independently
  with NO barrier; a throwing stage drops that item to null. Stage callbacks get
  (prev, originalItem, index).
- phase(title) / log(msg) — fire-and-forget progress. args — the run's input.

The script runs with NO permissions: those five plus `args` are its entire world —
no filesystem, no network, no imports. It must also be DETERMINISTIC: `Date.now()`,
`new Date()` with no argument, and `Math.random()` throw inside a workflow. Pass
timestamps in through `args` and vary agent prompts by index.

Subagent caps do NOT apply inside a workflow (its own semaphore runs 4 agents at
once), so queue as many calls as the job needs. Workflow agents get no context
beyond their prompt string. Workflows do not nest.

rerun replays unchanged agent() calls from the previous run's journal instantly and
re-runs only the calls whose prompt or options changed — so editing a script and
rerunning costs only the edited calls.
