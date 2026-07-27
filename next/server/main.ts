/**
 * The entry point: open the database, build the one `AppCtx`, serve.
 *
 * The invariant this holds is that **process wiring lives here and only here.**
 * Everything else in the tree receives what it needs as a parameter, so this is
 * the single file that opens a real database, constructs a real bus, reads the
 * environment, and binds a socket. That is the shape that makes the rest of the
 * system testable: `app.ts` exports `createHandler(ctx)`, and a test builds its own
 * ctx over an in-memory database and never runs this file at all.
 *
 * **Loopback only.** The listener binds `127.0.0.1` with no override, because there
 * is no auth layer and none is planned — no remote access, no tunnel (spec §17).
 * Binding anywhere else would silently publish an unauthenticated API that runs
 * arbitrary programs as the user.
 *
 * **Coexistence.** `BOUGH_PORT` moves the listener and `BOUGH_HOME` relocates the
 * whole data root (`paths.ts`), which is what lets this run beside the live install
 * without touching its database, artifacts or schedules (plan §2). The defaults are
 * the product's — port 4321, `~/.bough` — so at cutover nothing has to change; while
 * building, set both.
 */
import { spawnCaps } from "../agents/caps.ts";
import { createNoteDeliverer, noteOrphanedSubagents, postSystemNote } from "../agents/notes.ts";
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import {
  createDelegatingTurnStarter,
  delegationTier,
  delegationTurnDeps,
} from "../hostfn/delegate.ts";
import { createArtifactHostFn } from "../hostfn/artifact.ts"; // T6.6
import { createAskHostFn } from "../hostfn/ask.ts"; // T6.1
import { createFetchHostFn } from "../hostfn/fetch.ts"; // T6.5
import { createImageHostFn } from "../hostfn/image.ts"; // T6.4
import { jobs } from "../hostfn/jobs.ts";
import { createScheduleHostFn } from "../hostfn/schedule.ts"; // T6.3
import { createStateHostFn } from "../hostfn/state.ts"; // T6.2
import { dbPath, workflowsDir } from "../paths.ts";
import { startScheduleTicker, TICK_MS } from "../schedules.ts"; // T6.3
import { BASE_HOST_FNS, createTurnStarter } from "../turn/runner.ts";
import {
  createWorkflowHostFn,
  type WithWorkflowControl,
  type WorkflowControlDeps,
} from "../workflow/control.ts";
import { recoverOrphanedTurns } from "../turn/state.ts";
import type { AppCtx } from "../types.ts";
import { syncScriptMirrors } from "../workflow/journal.ts";
import { recoverOrphanedWorkflows } from "../workflow/run.ts";
import { structuredWorkflowCtx, type WithStructuredWorkflow } from "../workflow/schema.ts";
import { createHandler } from "./app.ts";
import type { WithTurnStarter } from "./sessions.ts";

const PORT = Number(Deno.env.get("BOUGH_PORT") ?? 4321);

const db = openDb();
const bus = new Bus();

const ctx: AppCtx & WithTurnStarter = {
  db,
  bus,
  // Absent = the global default. The provider-routed client is built per turn by
  // the runner from the resolved model id (`llm/client.ts`), so nothing is wired
  // here; the cheap tier (T10.1) is, when it exists. Until then every feature that
  // needs one degrades rather than failing, which is what `AppCtx`'s optional
  // fields are for.
  model: Deno.env.get("BOUGH_MODEL") ?? undefined,
};

// ── boot wiring: append below, same append-only discipline as the route table ──
// The schedule ticker (T6.3) and MCP shutdown (T7.1) each add one line here.
// Append; do not reorder, and do not fold another task's wiring into your own.

// T3.2 — background shells publish `job.spawned`/`job.exited` through the bus. The
// registry is process-wide because a job outlives the turn that started it, so it
// is constructed before the bus exists and wired here rather than at construction.
jobs.attachBus(bus);

// T2.2 — the seam `server/sessions.ts` reads off the ctx. Without it a posted
// message is persisted and announced and then nothing happens: the session sits
// with an unanswered user message and no turn. `survivingJobs` is what lets an
// interrupt name the background shells that deliberately outlive it (spec §9).
ctx.startTurn = createTurnStarter({
  survivingJobs: (sessionId) => jobs.runningIds(sessionId),
});

// T2.3 — before the listener binds, never after. A client that connected first
// would fetch a session that still looks busy and render a turn that died with the
// previous process (spec §5: a restart mid-turn marks it `orphaned` and the session
// recovers instead of hanging).
const orphaned = recoverOrphanedTurns(db, bus);
if (orphaned.length > 0) {
  console.log(
    `recovered ${orphaned.length} turn(s) orphaned by the previous process: ` +
      orphaned.map((o) => `${o.turnId}@${o.step}`).join(", "),
  );
}

// T4.3 — the delegation width caps. The ledger is process-wide because the budget
// it holds is tree-wide: four subagents running at once spans sessions, turns and
// requests, so a per-request instance would count nothing. Attaching the bus is the
// backstop, not the accounting: a launch path that reserved a slot and then threw,
// or a detached child nobody kept a promise for, gives its slot back when its
// `turn.finished` arrives — so a slot cannot leak out of a tree's budget and quietly
// cap it at three, then two, for the life of the process.
spawnCaps.attachBus(bus);

// T4.2 — delegation. The turn starter is REBUILT here rather than edited above,
// because boot wiring is append-only (plan §4): a task adds its lines at the end and
// never reaches back into another task's. This assignment supersedes the T2.2 one and
// carries the same `survivingJobs` seam, plus the delegation verbs — `agent`, `spawn`,
// `join`, `adopt` — bridged into every program, with the grant chosen per session from
// its lineage (a root may detach work; a subagent delegates blocking only; a depth-2
// subagent or a workflow agent not at all).
//
// `deliver` is deliberately unset: posting a detached child's report to its spawner as
// a system note is T4.4 (`agents/notes.ts`). Until it lands, a detached report is not
// pushed — the branch is still in the tree and `join()` still claims it in-band.
ctx.startTurn = createDelegatingTurnStarter({
  base: { survivingJobs: (sessionId) => jobs.runningIds(sessionId) },
});

// T4.4 — system notes, and the wake they carry. Three wirings, one rule
// (`agents/notes.ts`): a note starts a turn on an idle session and rides the queued
// drain on a busy one, and never a second concurrent turn either way.
//
// The notifier first: `jobs` is constructed at module load, long before anything
// that knows how to post a note exists, so `attachNotifier` is how a background
// shell's exit reaches the model at all (spec §9). Without this line a `[background]
// bg_N finished` note is formatted and dropped, and the model is never told.
jobs.attachNotifier((sessionId, text) => {
  postSystemNote(ctx, sessionId, text);
});

// The starter is REBUILT rather than edited above, same append-only discipline as
// the T4.2 block it supersedes: identical wiring, plus the `deliver` seam that was
// deliberately left unset there. From here a detached child's unclaimed report is
// posted to its spawner instead of waiting for a `join()` that may never come.
ctx.startTurn = createDelegatingTurnStarter({
  base: { survivingJobs: (sessionId) => jobs.runningIds(sessionId) },
  deliver: createNoteDeliverer(),
});

// The child a restart stranded. Its spawner is holding a promise that died with the
// previous process, so the note is the only record that reaches the thread — and it
// is RECORDED, not woken: recovery surfaces a restart rather than resuming it
// (`turn/state.ts`), and a server coming back must not start spending on its own.
await noteOrphanedSubagents(ctx, orphaned);

// T5.1 — workflow recovery, and for the same reason turns get it: a run's worker and
// every subagent turn it was driving died with the previous process, but its row still
// says `running`. Left alone, the run view shows a fan-out that will never advance and
// `rerun` refuses it as still live. Reconciled BEFORE the listener binds, and SURFACED
// rather than resumed — a server coming back must not restart someone's 200-agent
// audit on its own (spec §8, plan §6.15's rerun is the deliberate way back in).
const orphanedWorkflows = recoverOrphanedWorkflows(db, bus);
if (orphanedWorkflows.length > 0) {
  console.log(
    `recovered ${orphanedWorkflows.length} workflow(s) orphaned by the previous process: ` +
      orphanedWorkflows.join(", "),
  );
}

// T5.3 — structured agent output. `agent(prompt, {schema})` must resolve to a PARSED,
// VALIDATED object, and a mismatch must RETRY rather than hand the script junk (spec
// §8). That guarantee is a decorator over the `AgentRunner` the workflow engine takes
// as a parameter, so installing it here is what makes it the process default instead
// of something each call site has to remember: every `WorkflowCtx` built in this
// process goes through `ctx.workflowCtx` on its way to `startWorkflow`.
//
// The reader is the workflow start path — the `workflow.*` verb and its routes (T5.5)
// — which is not landed yet; a reader that finds the seam absent falls back to the
// identity, which is exactly the pre-T5.3 behavior. Filled here rather than inside
// `workflow/run.ts` because that is another task's file, and because process wiring
// lives in this file and only in this file (see the header).
(ctx as AppCtx & WithTurnStarter & WithStructuredWorkflow).workflowCtx = structuredWorkflowCtx;

// T5.5 — workflow lifecycle control and its REST surface. Two wirings.
//
// First, the control seam the routes read (`server/workflows.ts`). Production could
// fall back to this module's defaults, but the child seam is worth stating: a
// workflow agent's turn is a turn like any other, so its background shells must be
// reportable by the same registry as everyone else's, and it is given the `none`
// delegation tier — a workflow agent gets its prompt string and nothing else, and
// must not fan out further (spec §8, "workflows do not nest").
const workflowControl: WorkflowControlDeps = {
  child: () => ({
    turn: delegationTurnDeps("none", {
      base: { survivingJobs: (sessionId: string) => jobs.runningIds(sessionId) },
    }),
  }),
};
(ctx as AppCtx & WithTurnStarter & WithWorkflowControl).workflowControl = workflowControl;

// Second, the program-side verb. The starter is REBUILT rather than edited above,
// the same append-only discipline as the T4.2/T4.4 blocks it supersedes: identical
// wiring, plus `workflow` bridged into every TOP-LEVEL turn's program and named in
// the grant so the prompt documents it. Without this line `workflow.start(...)`
// exists in the spec, in the prompt section and in the protocol's name list, and
// rejects at runtime as "not available in this turn" — a whole milestone reachable
// only over HTTP.
//
// Gated by tier, not by kind: a subagent may not start a workflow (it would outlive
// the report its spawner is waiting on), and the prompt's workflow section is gated
// the same way, so the bridge and the grant agree.
ctx.startTurn = createDelegatingTurnStarter({
  base: {
    survivingJobs: (sessionId) => jobs.runningIds(sessionId),
    granted: [...BASE_HOST_FNS, "workflow"],
  },
  deliver: createNoteDeliverer(),
  extend: (turnCtx) =>
    delegationTier(db, turnCtx.sessionId) === "top"
      ? { workflow: createWorkflowHostFn(turnCtx, workflowControl) }
      : {},
});

// T5.4 — the journal's on-disk surface. A run mirrors its script to
// `~/.bough/workflows/<id>.js` when it starts, and `rerun` prefers that file over the
// stored row, because "edit the file, press r" is the entire iteration loop for a
// workflow (spec §8). The file is the half of that loop the database cannot hold: a
// fresh checkout, a cleaned `~/.bough`, or a database carried to another machine
// leaves every run with a rerun path and nothing to edit — and `rerun` then silently
// falls back to the stored script, replaying the edit away.
//
// So the missing mirrors are rewritten from their rows at boot. Idempotent and cheap:
// an existing file is never read, compared or rewritten, so a user's edit survives
// every restart. Bounded to the most recent runs — the mirror is an editing surface
// for work someone is still iterating on, not an export of every run ever made.
const mirrored = await syncScriptMirrors(db);
if (mirrored.length > 0) {
  console.log(`restored ${mirrored.length} workflow script mirror(s) under ${workflowsDir()}`);
}

// T6.3/T6.4/T6.5 — the session verbs `schedule.*`, `image()` and `fetch()`, and the
// ticker that makes a schedule actually recur.
//
// The starter is REBUILT rather than edited above, the same append-only discipline as
// every block before it: identical wiring, plus three more host functions merged into
// the `extend` seam and named in `granted` so `prompt/assemble.ts` includes their
// sections. Both halves are required and neither is sufficient — a turn told about
// `fetch()` that cannot call it wastes a round, and a turn that can call one it was
// never told about will not (spec §6: a host function exists only when the prompt
// grants it).
//
// Granted at every tier, unlike `workflow`: a subagent that renders a chart should be
// able to look at it, and one that reads an API should not have to shell out to curl.
// `schedule.*` rides along for the same reason — it manages rows, it does not fan out.
ctx.startTurn = createDelegatingTurnStarter({
  base: {
    survivingJobs: (sessionId) => jobs.runningIds(sessionId),
    granted: [...BASE_HOST_FNS, "workflow", "schedule", "image", "fetch"],
  },
  deliver: createNoteDeliverer(),
  extend: (turnCtx) => ({
    ...(delegationTier(db, turnCtx.sessionId) === "top"
      ? { workflow: createWorkflowHostFn(turnCtx, workflowControl) }
      : {}),
    ...createScheduleHostFn(turnCtx),
    ...createImageHostFn(turnCtx),
    ...createFetchHostFn(turnCtx),
  }),
});

// The ticker itself. Without this line a schedule is a row that is written, listed,
// enabled — and never fires, which is the whole feature missing with nothing to
// notice it by. Started AFTER the starter above is installed, so the first firing has
// a turn to run; its timer is unref'd (`schedules.ts`), so the stopper is discarded
// deliberately — the ticker never holds the process open and dies with it.
startScheduleTicker(ctx);
console.log(`schedule ticker running every ${TICK_MS / 1000}s`);

// T6.1/T6.2 — the session verbs `ask()` and `state.*`.
//
// The starter is REBUILT rather than edited above, the same append-only discipline as
// every block before it: identical wiring, plus two more host functions merged into
// the `extend` seam and named in `granted` so `prompt/assemble.ts` includes their
// sections. Both halves are required and neither is sufficient (spec §6: a host
// function exists only when the prompt grants it) — without the `extend` half,
// `ask()` and `state.get()` are documented in the prompt and reject at runtime as
// "not available in this turn"; without the `granted` half they are callable and
// never mentioned, so the model never reaches for them.
//
// Granted at every tier, like `schedule`/`image`/`fetch` and unlike `workflow`.
// `state` because the lineage root is precisely the scope a subagent shares with its
// spawner (`hostfn/state.ts`), so a delegate that cannot read the store cannot see the
// bookkeeping for the work it was handed. `ask` because a question is a question
// wherever it comes from — the hold carries its own session id, so the card lands on
// the branch that raised it and the human answers it there.
//
// Nothing else is wired for either: `ask` is memory-only by design and has no
// recovery pass at boot (a pending hold means nothing once its turn is gone, so a
// restart leaves nothing stale to heal), and `state` is plain rows behind the frozen
// `Db` accessors. The two routes are appended in `app.ts`.
ctx.startTurn = createDelegatingTurnStarter({
  base: {
    survivingJobs: (sessionId) => jobs.runningIds(sessionId),
    granted: [...BASE_HOST_FNS, "workflow", "schedule", "image", "fetch", "ask", "state"],
  },
  deliver: createNoteDeliverer(),
  extend: (turnCtx) => ({
    ...(delegationTier(db, turnCtx.sessionId) === "top"
      ? { workflow: createWorkflowHostFn(turnCtx, workflowControl) }
      : {}),
    ...createScheduleHostFn(turnCtx),
    ...createImageHostFn(turnCtx),
    ...createFetchHostFn(turnCtx),
    ...createAskHostFn(turnCtx),
    ...createStateHostFn(turnCtx),
  }),
});

// T6.6 — `artifact()`. The starter is REBUILT rather than edited above, the same
// append-only discipline as every block before it: identical wiring, plus `artifact`
// merged into the `extend` seam and named in `granted` so `prompt/assemble.ts`
// includes its section (`prompt/artifact.md`). Both halves are required and neither is
// sufficient — a turn told about `artifact()` that cannot call it wastes a round, and
// a turn that can call one it was never told about will not (spec §6).
//
// Granted at every tier, like `image`/`fetch` and unlike `workflow`: a subagent that
// renders a comparison should be able to publish it, and the store is per-session, so
// the link it reports resolves under its own directory with nothing to merge.
//
// Nothing else is wired for artifacts, and that is the point of the design: the
// FILESYSTEM is the source of truth (spec §4), so there is no table to open, no index
// to rebuild, and no recovery pass at boot. `~/.bough/artifacts/<sessionId>/` is
// created lazily on first publish, and `GET /sessions/:id/artifacts` walks whatever is
// there — including artifacts this process has never seen. T6.7's comment sidecars sit
// in `~/.bough/comments/`, OUTSIDE that tree (plan §6.12), so neither the listing nor
// the artifact route can reach them; they need no wiring here either. The five routes
// are appended in `app.ts`.
ctx.startTurn = createDelegatingTurnStarter({
  base: {
    survivingJobs: (sessionId) => jobs.runningIds(sessionId),
    granted: [
      ...BASE_HOST_FNS,
      "workflow",
      "schedule",
      "image",
      "fetch",
      "ask",
      "state",
      "artifact",
    ],
  },
  deliver: createNoteDeliverer(),
  extend: (turnCtx) => ({
    ...(delegationTier(db, turnCtx.sessionId) === "top"
      ? { workflow: createWorkflowHostFn(turnCtx, workflowControl) }
      : {}),
    ...createScheduleHostFn(turnCtx),
    ...createImageHostFn(turnCtx),
    ...createFetchHostFn(turnCtx),
    ...createAskHostFn(turnCtx),
    ...createStateHostFn(turnCtx),
    ...createArtifactHostFn(turnCtx),
  }),
});

// Teardown. Background shells are children of THIS process, so an unkilled one
// survives as an orphan with no reader for its output — kill children before the
// process goes, the same ordering rule the program worker holds (plan §6.3).
let shuttingDown = false;
function shutdown(signal: string): void {
  if (shuttingDown) return;
  shuttingDown = true;
  const killed = jobs.killAll();
  console.log(`${signal}: killed ${killed} background shell(s), closing db`);
  db.close();
  Deno.exit(0);
}
for (const signal of ["SIGINT", "SIGTERM"] as const) {
  try {
    Deno.addSignalListener(signal, () => shutdown(signal));
  } catch {
    // Not every platform exposes every signal; teardown is best-effort, and
    // failing to register one must not stop the server from starting.
  }
}

Deno.serve({
  port: PORT,
  hostname: "127.0.0.1",
  onListen: ({ hostname, port }) =>
    console.log(`bough listening on ${hostname}:${port} — db ${dbPath()}`),
}, createHandler(ctx));
