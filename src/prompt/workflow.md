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
  returns its report; throws on failure. With `schema` it returns the parsed,
  validated object — branch on typed data instead of parsing prose.

`schema` is a STRICT subset of JSON Schema, checked before anything bills, and a
schema that misses a rule fails EVERY call that uses it. The four that are easy to
miss:

- every object sets `additionalProperties: false` — including nested ones
- the root is `type: "object"`; wrap a bare array as `{items: [...]}`
- every subschema declares a `type`, and every array declares its `items`
- `required` lists every key in `properties`, and names nothing else

    {type: "object", additionalProperties: false,
     required: ["bugs"],
     properties: {bugs: {type: "array", items: {
       type: "object", additionalProperties: false,
       required: ["file", "why"],
       properties: {file: {type: "string"}, why: {type: "string"}}}}}}
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

Subagent caps do NOT apply inside a workflow (its own semaphore runs up to 16 at
once, fewer on a small machine), so queue as many calls as the job needs. Workflow
agents get no context beyond their prompt string. Workflows do not nest.

What the script RETURNS is the report that arrives in the system note. A script that
writes its findings to a file and returns nothing produces `Result: null`, and the
work has to be fetched back with a second `workflow.status({id})` round-trip —
return the summary itself.

rerun replays unchanged agent() calls from the previous run's journal instantly and
re-runs only the calls whose prompt or options changed — so editing a script and
rerunning costs only the edited calls.
