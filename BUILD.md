# Build ledger

Phase plan: REQUIREMENTS.md §17. One row per phase; a phase is DONE only when every item of its
"Verify:" list has a named test (or a named manual gate) and `make gates` is green.

| Phase | What | Status | Verified by | Deferred / deviations |
|---|---|---|---|---|
| 0 | the center: kernel, launcher, util, bough-base with one row | in progress | | |
| 1 | the ledger + projection seam | pending | | |
| 2 | one resident agent end to end | pending | | |
| 3 | the TUI, old-feed adapter, FTS pane — interface cutover gate | pending | | gate "one full real workday" is Andrey's act |
| 4 | memory: rollups, reconsolidation, drift-watch | pending | | |
| 5 | many agents, leader, graph-ops | pending | | |
| 6 | collectors, mcp, actions providers | pending | | real outward acts (open a PR) are verified against a recording `gh` shim, never live |
| 7 | wards-rhai, hooks-exec, mcp-subprocess, skills, sleep-listener, idle ticks | pending | | |
| 8 | digging + hardening + everything-is-a-plugin audit | pending | | |

## Standing assumptions taken by the build (flag to Andrey)

- `sol` / `terra` (§12) are config fields of `model-policy`; bough-base sets sol=`claude-opus-5`,
  terra=`claude-sonnet-5`. Change by patch.
- `~/.jungler/jungler.db` does not exist on this machine (jungler is a design repo, not a daemon);
  the Phase 3 adapter reads it only if present and activates with zero jungler mail otherwise.
- The worktree is `~/repos/bough-rebuild`; `~/repos/bough` (main) is never modified by the rebuild.
