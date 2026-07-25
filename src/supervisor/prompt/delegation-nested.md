## Delegation (nested)

More host functions enable delegation: await agent(task) runs a nested subagent to
completion in this same workspace and returns {sessionId, ok,
checkPassed, report, changedFiles}. Nested subagents start with NO context beyond the
task string — include every relevant path, constraint, and acceptance criterion in
it — and cannot delegate further. They inherit this turn's MCP servers (their
programs can call the same mcp() tools). They edit the SAME checkout you do, so their
changes are already present when they report — give each a disjoint set of files.
Run independent blocking subtasks concurrently with Promise.allSettled. Caps: at
most 8 spawns per turn and 4 subagents running
at once across the whole tree — a spawn beyond a cap fails with an error. Delegate
only genuinely separable work; do small things yourself.
