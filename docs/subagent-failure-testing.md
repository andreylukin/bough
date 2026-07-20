# Testing subagent failure & interruption

The subagent lifecycle has many ways to go wrong — the turn errors, the user
interrupts, it times out, a launch is refused, the server restarts mid-flight —
and each flows back to the parent (and the UI) very differently. This is the
plan for testing all of it thoroughly, the test tiers, and the current coverage.

## How outcomes flow back (the surface under test)

- **Blocking `agent()` / `join()`** — the result `{ok, checkPassed, report,
  changedFiles}` returns *in-band* to the parent's program. There is **no system
  note and no UI card update**; the only trace is what the parent does with `ok`.
- **Detached `spawn()`** — on finish, `formatNote()` posts a `[subagent
  finished] …` system note that wakes the parent and renders a card.
- `ok = (turn.status === "done")`; `checkPassed` = the harness accepted a
  committed check. So *finished-but-unverified* (`ok:true, checkPassed:false`) is
  distinct from *failed* (`ok:false`).

## Failure / interruption taxonomy

| id | scenario | expected |
| --- | --- | --- |
| A1 | blocking `agent()` turn errors | `ok:false`, error carried in `report` |
| A2 | detached `spawn()` errors | `FAILED` note posted, wakes parent, red card |
| A3 | `join()` on an errored detached | in-band `ok:false`, no note |
| A4 | subagent finishes without a check | `ok:true`, `checkPassed:false` (partial) |
| A5 | nested subagent (depth 2) errors | propagates up to root |
| B1 | interrupt spawner while `agent()` blocks | both stop; spawner program is killed, subagent `interrupted` |
| B2 | interrupt spawner while `join()` waits | same as B1 |
| B3 | interrupt spawner with a detached `spawn()` live | subagent **survives** (not tied to the signal) |
| B4 | subagent overruns the turn timeout | auto-interrupt, `ok:false` |
| B5 | user opens a running subagent, hits esc | goes **back to spawner** (does not stop it) — no stop path for a runaway detached child |
| C1–C3 | depth / spawn / concurrency cap | `agent()`/`spawn()` rejects with a clear error the model reads |
| C4 | workspace-branch failure | session archived + clean error |
| C5 | empty task | rejects, no phantom subagent |
| C6 | `Promise.all` fan-out where one launch throws | batch rejects, a live sibling is stranded |
| D1 | server restart mid-subagent | orphan surfaced in its own thread; **does the parent get woken?** |
| D2 | spawner archived while subagent runs | `postSystemNote` to a dead session |
| D3 | `adopt()` a failed/interrupted subagent | currently adopts partial changes with no ok-check |
| D4 | `adopt()` a still-running subagent | adopting a live branch |
| E1 | failed *blocking* subagent card | **renders green "✓ done"** (Branch has no status) |
| E2 | failed *detached* subagent card | red, via the `FAILED` note |
| E3–E6 | interrupted card, live running→failed transition, long-error truncation, click-into-failed then esc-back | |
| F1–F3 | parent handling of `ok:false`, the wake-note next action, mixed success/failure fan-out | |

## Test tiers

**Tier 1 — unit, scripted fake LLM** (`src/subagent.test.ts`). The workhorse:
deterministic, fast, no network. The `dispatchLlm` harness keys scripted rounds
by thread text; a round is a value or a thunk that now receives the turn's abort
`signal`. Failure helpers: `errorRound(msg)` (rejects like an LLM error),
`abortRound(onStart)` (pends until the turn is aborted, then rejects like a
cancelled request). Drive with `beginTurn` + `await done`; trigger interrupts
with `interruptTurn(id)`; shrink the timeout with `BOUGH_SUBAGENT_TIMEOUT_MS`.

**Tier 2 — integration, real isolated server** (`bench/server.sh`-style). Boot a
scratch-DB/scratch-port server, drive a real turn (`bough exec`) that spawns a
subagent scripted to fail (a command that can't pass a check, or a `sleep` you
interrupt via `POST /sessions/:id/interrupt`), then assert `turnForMessage`
status, the `[subagent finished]` note, and the shadow-branch state. This is the
only tier that covers D1 (kill + restart mid-subagent → `recoverOrphanedTurns`)
and the real-git adopt paths (D3/D4).

**Tier 3 — TUI, shell-use + seeded DB** (as in the subagent-card work). Seed the
DB with each terminal state (running / errored-blocking / errored-detached-with-
note / interrupted / orphaned) — no LLM needed — launch the TUI against an
isolated server, and screenshot/assert. This catches the rendering gaps (E1–E6).
`buildLines` in `src/tui/lines.ts` is also unit-testable directly (see the E1
test) without a live TUI.

## Current coverage & status

Tested (Tier 1 in `subagent.test.ts`, plus E1 in `tui/lines.test.ts`, D1 unit +
live-verified): A1, A2, A4, B1, B3, B4, C1–C3 (caps), C5, C6, D1, E1.

Gaps the tests surfaced — **now fixed** (each with a test that flipped from
pinning the bug to asserting the fix):
- **E1 (fixed)** — `Branch` gained a `status` (from the session's
  `lastTurnStatus`); `branchCardLines` renders `✗ failed` / `◼ interrupted`
  instead of a blanket green `✓ done`. Live-verified in the TUI. `store.reload`
  now prefers the server's authoritative `lastTurnStatus`.
- **A2 (fixed)** — `formatNote` (and `SubagentResult.status`) distinguish
  `FAILED — its turn errored`, `STOPPED — interrupted`, and `ORPHANED`;
  `parseSubagentNote` treats all three as not-ok.
- **C6 (fixed)** — the delegation prompt now recommends `Promise.allSettled`, and
  the test proves it preserves a good sibling's result when another launch fails.
- **B5 (fixed)** — an explicit interrupt of the spawner now cascades to its
  detached subagents (a `turn.ts` interrupt-hook registry), so a runaway detached
  child is stoppable; a *normal* turn end still lets it survive.
- **D1 (fixed)** — `recoverOrphanedTurns` posts an `ORPHANED` note into the
  spawner's thread, so a subagent stranded by a restart isn't silently lost.
- **B1** — pinned as intended behavior: interrupting the spawner kills its
  program too (stop = stop now); no fix needed.

Tier 3 (shell-use, `probes/subagent-failures.sh` — isolated server + seeded DB,
no tokens): E3 interrupted card reads `◼ interrupted`; E4 a run stranded by a
restart renders `ORPHANED` + posts a note to the spawner; E5 a long-error report
card is capped and expands on "click to show all"; E6 clicking a failed card
opens the subagent (its Turn-failed text) and esc returns to the spawner. Run:
`bash probes/subagent-failures.sh`.

Run the unit tier:

```sh
deno test --unstable-worker-options -A src/subagent.test.ts src/tui/lines.test.ts
```
