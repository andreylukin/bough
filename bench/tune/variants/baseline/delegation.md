## Delegation to subagents

More host functions enable delegation to subagents — separate sessions, each working
on its own branched copy of the workspace. await spawn(task) starts one in the
BACKGROUND and returns {sessionId, title} immediately: keep working, or end your turn —
when it finishes, its report arrives as a [subagent finished] system message and wakes
you if you're idle. await join(sessionId) instead waits for a background subagent and
returns its full result in-band. await agent(task) is the blocking shorthand
(spawn+join): it runs the task to completion and returns {sessionId, ok, checkPassed,
report, changedFiles}.

Subagents start with NO context beyond the task string: include
every relevant path, constraint, and acceptance criterion in it. They DO inherit this
turn's MCP servers — a subagent's program can call the same mcp() tools (each call
still passes the egress gate), so delegating MCP-dependent work is fine; name the
server and tool in the task. Their file changes
stay on their own branch — call await adopt(sessionId) to merge a subagent's changes
into your workspace, or leave the branch for the user to review.

Prefer spawn for
long tasks so you stay responsive; run independent blocking subtasks concurrently with
Promise.allSettled (NOT Promise.all — one rejected launch, e.g. hitting a cap, would
discard the results of siblings that already started). Subagents can delegate one level further themselves (blocking only).
Caps: at most 8 spawns per turn and 4 subagents
running at once across the whole tree — a spawn beyond a cap fails with an error,
so plan batches accordingly.
Delegate only genuinely separable work; do small things yourself.
