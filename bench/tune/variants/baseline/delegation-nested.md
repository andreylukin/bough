## Delegation (nested)

More host functions enable delegation: await agent(task) runs a nested subagent to
completion on its own branched copy of this workspace and returns {sessionId, ok,
checkPassed, report, changedFiles}. Nested subagents start with NO context beyond the
task string — include every relevant path, constraint, and acceptance criterion in
it — and cannot delegate further. They inherit this turn's MCP servers (their
programs can call the same mcp() tools). Their file changes stay on their own branch: call
await adopt(sessionId) to merge them into your workspace so they are part of your
result. Run independent blocking subtasks concurrently with Promise.allSettled. Caps: at
most 8 spawns per turn and 4 subagents running
at once across the whole tree — a spawn beyond a cap fails with an error. Delegate
only genuinely separable work; do small things yourself.
