# Build ledger

Phase plan: REQUIREMENTS.md §17. One row per phase; a phase is DONE only when every item of its
"Verify:" list has a named test (or a named manual gate) and `make gates` is green.

| Phase | What | Status | Verified by | Deferred / deviations |
|---|---|---|---|---|
| 0 | the center: kernel, launcher, util, bough-base with one row | done | `make gates` green (254 tests). Per §17 verify item: registers/injects/activates/unloads/reloads — `bough-plugin-hello` `tests/lifecycle.rs` (7); effects LIFO — `lifecycle::hello_effects_unwind_lifo_on_unload` + `effect::tests::*`; waterfall short-circuit — `event::tests::waterfall_short_circuits_when_next_is_skipped`; scoped shadowing — `scope::tests::scoped_service_shadows_global_for_that_key_only`; `--dump-config` equals boot — `bough` `tests/dump_config.rs` (4); bad patch — `bough` `tests/bad_patch.rs` (3); undeclared key — `bough-plugin-hello` `tests/undeclared.rs`; invariant runner — `bough` `tests/invariants.rs` (2); SWAP gate — `bough` `tests/swap.rs` (4) | A RELOAD keeps the `FiberUid`; only a `plugin`/`id` change rebuilds (REQUIREMENTS §0.3 line 107) — `swap.rs` asserts the reload on the trace, not on a moved uid. `Cadence::Interval`/`OnEvent` are declared but only `OnQuiesce` dispatches. `quiesce()` is a stability poll (3 clean passes, 10s ceiling), not an edge-triggered barrier; the UNLOADING dependent wait has a 5s ceiling that logs and proceeds. Decision D18 (a row may omit `plugin:`) is NOT implemented. `config-update-failed` on a candidate that fails to COMPOSE is emitted by the launcher's watch path, not the kernel. `!!expr` is rewritten to the local tag `!expr` before serde_yaml sees it (0.9 discards unknown `!!` tags). |
| 1 | the ledger + projection seam | pending | | |
| 2 | one resident agent end to end | pending | | |
| 3 | the TUI, old-feed adapter, FTS pane — interface cutover gate | pending | | gate "one full real workday" is Andrey's act |
| 4 | memory: rollups, reconsolidation, drift-watch | pending | | |
| 5 | many agents, leader, graph-ops | pending | | |
| 6 | collectors, mcp, actions providers | pending | | real outward acts (open a PR) are verified against a recording `gh` shim, never live |
| 7 | wards-rhai, hooks-exec, mcp-subprocess, skills, sleep-listener, idle ticks | pending | | |
| 8 | digging + hardening + everything-is-a-plugin audit | pending | | |

## Standing assumptions taken by the build (flag to Andrey)

- `sol` / `terra` (§12) are config fields of `model-policy`; during the build BOTH are
  `claude-haiku-4-5-20251001` (Andrey: haiku for testing in the beginning). Change by patch.
- `~/.jungler/jungler.db` does not exist on this machine (jungler is a design repo, not a daemon);
  the Phase 3 adapter reads it only if present and activates with zero jungler mail otherwise.
- The worktree is `~/repos/bough-rebuild`; `~/repos/bough` (main) is never modified by the rebuild.
