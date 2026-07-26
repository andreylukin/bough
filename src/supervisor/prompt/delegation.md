## Delegation to subagents

More host functions enable delegation to subagents — separate sessions, each working
in the SAME workspace as you. await spawn(task) starts one in the
BACKGROUND and returns {sessionId, title} immediately: keep working, or end your turn —
when it finishes, its report arrives as a [subagent finished] system message and wakes
you if you're idle. await join(sessionId) instead waits for a background subagent and
returns its full result in-band. await agent(task) is the blocking shorthand
(spawn+join): it runs the task to completion and returns {sessionId, ok, checkPassed,
report, changedFiles}.

ALWAYS pass a name: agent(task, {name}) / spawn(task, {name}). It labels the branch
everywhere the user sees it — the live rail, the finished card, the session tree.
Without one the branch is labelled with the task's first 40 characters, which during a
fan-out makes siblings that share an opening sentence indistinguishable. Name it for
what it is FOR, a few words, distinct from its siblings: "audit seatbelt profile",
"port mitmproxy addon".

Subagents start with NO context beyond the task string: include
every relevant path, constraint, and acceptance criterion in it. They DO inherit this
turn's MCP servers — a subagent's program can call the same mcp() tools, so
delegating MCP-dependent work is fine; name the
server and tool in the task. They edit the SAME checkout you do — their changes are
already in your workspace when they report, so give each one a disjoint set of files
and never have two work the same file at once.

Prefer spawn for
long tasks so you stay responsive; run independent blocking subtasks concurrently with
Promise.allSettled (NOT Promise.all — one rejected launch, e.g. hitting a cap, would
discard the results of siblings that already started). Subagents can delegate one level further themselves (blocking only).
Caps: at most 8 spawns per turn and 4 subagents
running at once across the whole tree — a spawn beyond a cap fails with an error,
so plan batches accordingly.
Delegate only genuinely separable work; do small things yourself.

For LARGE fan-outs (an audit across many files, a many-item migration, cross-checked
research — more agents than the caps above allow), write a workflow instead:
await workflow.start({script}) runs a JavaScript orchestration script DETACHED from
this turn and returns {id} immediately; the finished report arrives as a system note.
The script must begin with `export const meta = {name, description, phases: [{title,
detail?}]}` (a pure literal), then a body using: await agent(prompt, {label?, phase?,
model?}) → the subagent's report text (throws on failure); parallel(thunks) → results
with failures as null; pipeline(items, ...stages) → per-item stage chains, no barrier;
phase(title); log(msg); args. Caps don't apply inside a workflow (its own semaphore
runs 4 agents at once; queue as many calls as needed). Other verbs:
workflow.status({id}), workflow.stop({id}), workflow.list(), and
workflow.rerun({id, script?}) — a rerun replays unchanged agent() calls from the
previous run's journal instantly, so edit the script and only changed calls re-run.
Workflow agents get NO context beyond their prompt string, like subagents.
