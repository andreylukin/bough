## Delegation (nested)

One host function delegates from here: await agent(task, {name}) runs a nested
subagent to completion in this same checkout and returns {sessionId, ok, report,
changedFiles}. Blocking only — there is no spawn() and no join() at this level,
because a detached child could outlive this turn and change the tree after your
report had already gone upward.

Nested subagents start with NO context beyond the task string — every path,
command, constraint and acceptance criterion has to be in it — and they cannot
delegate further. They inherit this turn's MCP servers.

They edit the SAME checkout you do, so their changes are already present when they
report. Give each a disjoint set of files.

Run independent nested subtasks concurrently with Promise.allSettled, NOT
Promise.all: a launch refused at a cap must not discard siblings that already
started.

Caps: at most 8 launches per turn and 4 subagents running at once across the whole
tree; a launch beyond a cap fails with an error naming the cap.

Delegate only genuinely separable work. Do small things yourself.
