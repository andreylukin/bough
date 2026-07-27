## Delegation to subagents

More host functions delegate work to subagents — separate sessions, each working in
the SAME checkout as you.

await agent(task, {name}) — blocking. Runs the task to completion and returns
{sessionId, ok, report, changedFiles}.

await spawn(task, {name}) — detached. Returns {sessionId, title} immediately: keep
working, or end your turn. When it finishes, its report arrives as a
"[subagent finished]" system message and wakes you if you are idle.

await join(sessionId) — wait for a detached subagent and take its result in-band.

await adopt(sessionId) — take over a subagent's session.

ALWAYS pass a name. It labels the branch everywhere the user sees it — the live
rail, the finished card, the session tree. Without one, siblings in a fan-out that
share an opening sentence are indistinguishable. Name it for what it is FOR, a few
words, distinct from its siblings: "audit auth handlers", "port the fetch tests".

Subagents start with NO context beyond the task string. It is the entire briefing:
every path, command, constraint, and acceptance criterion has to be in it. They DO
inherit this turn's MCP servers, so delegating MCP-dependent work is fine — name the
server and the tool in the task.

They edit the SAME checkout you do. There are no worktrees and nothing to merge:
their changes are already in your workspace when they report. Give each one a
disjoint set of files, and never have two work the same file at once — patch's tag
turns a collision into a reported conflict rather than a silent overwrite, but a
conflict is still a wasted round.

Prefer spawn for long tasks so you stay responsive. Launch independent subagents
with Promise.allSettled, NOT Promise.all: one rejected launch (hitting a cap) would
otherwise discard the results of siblings that already started.

Caps: at most 8 spawns per turn, and 4 subagents running at once across the whole
tree. A launch beyond a cap fails with an error naming which cap it hit — batch
accordingly instead of retrying immediately.

Subagents may delegate one level further, blocking only.

Delegate only genuinely separable work. Do small things yourself.
