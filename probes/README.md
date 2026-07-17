# TUI usability probes

Scripted measurements of how well the bough harness collaborates with a human,
grounded in the human-agent-interaction literature (HALIE process metrics,
CollabSkill's harness-not-model result, Claude Code's observable-autonomy
design). Run the same battery before and after a harness change; improvements
should be numbers, not anecdotes.

Each probe drives the **real TUI against the live server** with
[shell-use](https://github.com/microsoft/shell-use), from a scratch workspace
outside the repo, and archives its conversation afterward. Probes spend real
tokens (one short turn each).

| Probe | Metric | Why it matters |
| --- | --- | --- |
| `first-output.sh` | submit → first visible output (end-to-end + server-side) | blank-turn time; a user can only interrupt what they can see |
| `interrupt.sh` | Esc mid-stream → "⏹ Stopped." + clean server state | cost of pulling the brake must stay far below the cost of undoing |
| `chrome.sh` | fixed chrome markers present (`›` composer, `? help`) | wrappers and humans both navigate by stable chrome |
| `scrollback-dump.sh` | full scrollback dump for the reconstruction test | scrollback is the chronological record; what a fresh reader can't reconstruct, the rendering lost |
| `report.sh` | per-session metrics table (`GET /sessions/:id/metrics`) | prompts-per-task, turns-to-done, stops/fails, latency percentiles across real usage |

Server-side metrics come from `src/metrics.ts` — derived from rows the server
already persists, plus a `turns.first_output_at` stamp written by the turn
runner the moment anything becomes visible.

The reconstruction test: run any real task, then
`./scrollback-dump.sh > /tmp/dump.txt` and hand the dump to a fresh agent with
no other context: "describe what the agent did and why". Misses and
misorderings point at rendering information loss.
