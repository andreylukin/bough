# Delegation

Two mechanisms, chosen by size. Subagents are for a handful of separable tasks inside a
turn; workflows are for fan-outs bigger than the subagent caps allow.

## Subagents

Separate sessions, each working in the **same checkout**.

| | |
|---|---|
| `await agent(task, {name})` | Blocking. Runs to completion, returns `{sessionId, ok, report, changedFiles}`. |
| `await spawn(task, {name})` | Detached. Returns `{sessionId, title}` immediately; the report arrives later as a `[subagent finished]` system message and wakes an idle conversation. |
| `await join(sessionId)` | Wait for a detached subagent and take its result in-band. |
| `await adopt(sessionId)` | Take over a subagent's session. |

**A subagent starts with no context beyond the task string.** It is the entire briefing,
every path, command, constraint and acceptance criterion has to be in it. They do inherit
the turn's MCP servers.

**They share your checkout.** There are no worktrees and nothing to merge: their changes
are already in the workspace when they report. Give each a disjoint set of files.
`patch`'s tag turns a collision into a reported conflict rather than a silent overwrite,
but a conflict is still a wasted round.

**Caps:** at most 8 spawns per turn, and 4 subagents running at once across the whole
tree. Exceeding one fails with an error naming which cap it hit. Subagents may delegate
one level further, blocking only.

Names matter more than they look: the name labels the branch in the live rail, the
finished card and the session tree. Siblings in a fan-out that share an opening sentence
are otherwise indistinguishable.

In the TUI, `esc` from inside a subagent returns to the session that spawned it.

## Workflows

A workflow is a JavaScript orchestration script that runs **detached from the turn**.

```js
export const meta = {
  name: 'review-changes',
  description: 'Review changed files across dimensions',
  phases: [{ title: 'Review' }, { title: 'Verify' }],
}

const results = await pipeline(
  DIMENSIONS,
  d => agent(d.prompt, { label: `review:${d.key}`, phase: 'Review', schema: FINDINGS }),
  review => parallel(review.findings.map(f => () =>
    agent(`Verify: ${f.title}`, { phase: 'Verify', schema: VERDICT }))),
)
return { confirmed: results.flat().filter(Boolean) }
```

`meta` must be a **pure literal**: no variables, calls or interpolation. It is read
host-side before the body runs.

The body gets five primitives and nothing else:

| | |
|---|---|
| `agent(prompt, {label?, phase?, model?, schema?})` | Runs a subagent, returns its report; throws on failure. With `schema`, returns a parsed and validated object. |
| `parallel(thunks)` | A barrier that awaits all. Failures come back as `null`; it never rejects. |
| `pipeline(items, ...stages)` | Each item flows through every stage independently, **no barrier**. A throwing stage drops that item to `null`. |
| `phase(title)` / `log(msg)` | Fire-and-forget progress. |
| `args` | The run's input. |

Prefer `pipeline`. A barrier is only correct when a stage genuinely needs cross-item
context from all of the previous one.

**The script has no permissions.** Those five plus `args` are its entire world: no
filesystem, no network, no imports. It must also be deterministic: `Date.now()`,
argless `new Date()` and `Math.random()` throw, because journal replay depends on it.
Pass timestamps in through `args`.

**Subagent caps do not apply inside a workflow.** Its own semaphore runs up to 16 at
once, fewer on a small machine.

**What the script returns is the report.** A script that writes findings to a file and
returns nothing produces `Result: null` and costs a second round-trip to fetch back.

### Journal and rerun

Every `agent()` call is journaled before it runs. A stopped run loses no completed work,
and `rerun` replays unchanged calls from the journal instantly, re-running only the calls
whose prompt or options changed, so editing a script and rerunning costs only the edits.

Verbs: `workflow.start({script, args?})`, `.status({id})`, `.stop`, `.pause`, `.resume`,
`.list()`, `.rerun({id, script?})`. Runs are visible in the TUI under `^w`.

Workflows do not nest.
