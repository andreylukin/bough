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
import { killAllMcpServers } from "../mcp/client.ts"; // T7.1
import { loadRegistry, registryFile } from "../mcp/config.ts"; // T7.1
import { authStatus, callbackUrl, configureOAuthCallback } from "../mcp/oauth.ts"; // T7.2
import { createMcpHostFns } from "../hostfn/mcp.ts"; // T7.3
import { bindTurnGrant, mcpManager } from "../mcp/manager.ts"; // T7.3
import { createScheduleHostFn } from "../hostfn/schedule.ts"; // T6.3
import { createStateHostFn } from "../hostfn/state.ts"; // T6.2
import { createLspHostFn } from "../hostfn/lsp.ts"; // T7.4
import { BACKEND_NAME, BIN_ENV_VAR, findBackend, lspAvailable } from "../lsp/lsp.ts"; // T7.4
import { assemblePrompt } from "../prompt/assemble.ts"; // T7.4
import { dbPath, workflowsDir } from "../paths.ts";
import { startScheduleTicker, TICK_MS } from "../schedules.ts"; // T6.3
import { BASE_HOST_FNS, createTurnStarter } from "../turn/runner.ts";
import {
  createWorkflowHostFn,
  type WithWorkflowControl,
  type WorkflowControlDeps,
  workflowCtxFor, // T5.7
  workflowCtxModel, // T5.7
} from "../workflow/control.ts";
import type { WithRelaunch } from "../workflow/relaunch.ts"; // T5.7
import { recoverOrphanedTurns } from "../turn/state.ts";
import type { AppCtx, TurnCtx } from "../types.ts";
import { // T10.2
  type ActiveSkills,
  defaultSources,
  listSkills,
  turnSkills,
  widenGrant,
} from "../skills/skills.ts";
import { syncScriptMirrors } from "../workflow/journal.ts";
import { guidelineAdvice } from "../workflow/report.ts"; // T5.8
import {
  MAX_AGENTS_PER_RUN,
  recoverOrphanedWorkflows,
  workflowConcurrency,
} from "../workflow/run.ts";
import { ensureSavedDir, savedDir } from "../workflow/saved.ts"; // T5.8
import { structuredWorkflowCtx, type WithStructuredWorkflow } from "../workflow/schema.ts";
import { cheapActivity, watchActivity } from "../worker/activity.ts"; // T10.1
import { cheapGhost } from "../worker/ghost.ts"; // T10.1
import { CHEAP_MODEL_ENV, cheapModel, cheapTitle, watchTitles } from "../worker/titles.ts"; // T10.1
import { createHandler } from "./app.ts";
import { indexRecoveredMessages, searchSafeDb } from "./search.ts"; // T8.6
import type { WithTurnStarter } from "./sessions.ts";

const PORT = Number(process.env["BOUGH_PORT"] ?? 4321);

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
  model: process.env["BOUGH_MODEL"] ?? undefined,
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

// T5.7 — relaunching a workflow from a stopped run's journal. One wiring, and the
// route is inert without it.
//
// The engine seeds a new run from an old one's journal on its own; what it cannot do
// is put REAL subagents behind the relaunched script. `workflow/relaunch.ts` is
// reachable from the route table, so importing the control layer from inside it would
// close a cycle through `server/app.ts` — the seam is filled here instead, with the
// same `workflowControl` deps the T5.5 block wired for start and rerun, so a
// relaunched run gets the identical launch context, caps exemption and
// structured-output decorator as the run it replaces.
//
// `effectiveModel` is not decoration. The journal key hashes the RESOLVED model, so a
// relaunch that resolved it any other way than the original run did would miss every
// key and re-run a 40-agent audit at full price while reporting a successful replay.
// Both halves come from `workflow/control.ts` for exactly that reason: one resolution,
// one place.
(ctx as AppCtx & WithTurnStarter & WithRelaunch).relaunch = {
  ctxFor: (c, sessionId) => workflowCtxFor(c, sessionId, workflowControl),
  effectiveModel: workflowCtxModel,
};

// T5.8 — saved workflows and the cost surface, wired before the listener binds.
//
// The directory first. `~/.bough/workflows/saved/` is meant to be a place a user can
// drop a script into and invoke by name, which is only true if it EXISTS before anyone
// looks — a directory that materializes on the first API save is one nobody finds.
// Best-effort, like the script mirrors above: a read-only `~/.bough` must not stop the
// server from starting, and saving will report its own error when it is tried.
const savedCount = await ensureSavedDir();
console.log(
  `${savedCount} saved workflow(s) under ${savedDir()} — ` +
    `POST /saved-workflows/<name>/runs invokes one with {sessionId, args}`,
);

// The size guideline is READ at view time (`workflow/report.ts`), never cached, so
// there is nothing to construct here. It is logged because it is the one workflow
// setting that changes what the model is told to aim for, and a boot line is how a user
// discovers that the run they are about to start will be flagged as large. Advisory in
// both directions: nothing below this line can pause, throttle or refuse a run, and the
// concurrency and lifetime numbers are printed beside it so the three are read together
// (spec §8's "Cost").
console.log(
  `${guidelineAdvice()} Up to ${workflowConcurrency()} agents at once, ` +
    `${MAX_AGENTS_PER_RUN}-agent lifetime backstop per run.`,
);

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
  process.exit(0);
}
for (const signal of ["SIGINT", "SIGTERM"] as const) {
  try {
    process.on(signal, () => shutdown(signal));
  } catch {
    // Not every platform exposes every signal; teardown is best-effort, and
    // failing to register one must not stop the server from starting.
  }
}

const server = Bun.serve({
  port: PORT,
  hostname: "127.0.0.1",
  // `Deno.serve` had no request deadline; `Bun.serve` defaults to 10 SECONDS and
  // then tears the connection down. Every long-lived thing bough has is longer than
  // that — the `/events` SSE stream is idle by design between turns, and one `bough
  // exec` request is held open for the whole turn (default 900s) — so the default
  // would show up as the TUI redialing every ten seconds and turns dying mid-flight.
  // 0 disables it, which is the semantics the code was written against.
  idleTimeout: 0,
  fetch: createHandler(ctx),
});
console.log(`bough listening on ${server.hostname}:${server.port} — db ${dbPath()}`);

// T7.1 — MCP. Two lines, and only the second one is required.
//
// The registry is a FILE, read fresh on every use (`mcp/config.ts`): MCP state is
// never cached, because grants and connections change between turns and a cached
// catalog is how the model ends up calling a tool that was revoked two turns ago
// (plan §6.13). So there is nothing to construct here and nothing to recover — the
// count is logged because the registry lives outside the database, and a server the
// user registered by editing the file is otherwise invisible until a turn needs it.
try {
  const registered = Object.keys(loadRegistry().servers);
  console.log(
    `${registered.length} MCP server(s) registered in ${registryFile()}` +
      (registered.length > 0 ? `: ${registered.join(", ")}` : ""),
  );
} catch (error) {
  // Best-effort, like the workflow mirrors above: an unreadable registry must not
  // stop the server from starting. The turn that needs a server will say so itself.
  console.log(`could not read ${registryFile()}: ${error}`);
}

// The required half: stdio servers are CHILDREN of this process, and killing the
// server has to take them with it. Same trap as background shells (plan §6.3): a
// chatty server dies of SIGPIPE when our end of its stdout closes, but a silent one
// — an idle HTTP bridge, a server between calls — survives, reparented and
// invisible, with nothing left that knows it exists.
//
// Hung on process `exit` rather than on the signal handlers above for two reasons. The
// handler that is already registered calls `process.exit` synchronously, so a second
// signal listener added here would never run; and `exit` also covers the exits no
// signal announces. `process.exit` fires it, so both paths converge here.
process.on("exit", () => {
  const killed = killAllMcpServers();
  if (killed > 0) console.log(`shutdown: killed ${killed} MCP server subprocess(es)`);
});

// T7.2 — remote MCP servers and OAuth. One required wiring, and it is required for
// a reason that only shows up at the end of the flow.
//
// bough is a public PKCE client that hosts its OWN redirect: the authorization
// server sends the user's browser back to `/mcp/oauth/callback` on this port. That
// URI is registered with the authorization server at dynamic-registration time and
// baked into every authorization request, so if it names a port nothing is
// listening on, the user approves access in their browser and lands on a connection
// error with no way back — and the failure appears at the very last step, after the
// registration is already stored. Pinning it to the port the listener actually bound
// is what keeps the redirect and the socket from drifting apart (`BOUGH_PORT` moves
// both; without this line only the socket moves).
//
// Nothing else is constructed here and nothing is recovered. Credentials are per-server
// files under `~/.bough/mcp/tokens/` read fresh on every use, for the same reason the
// registry is (plan §6.13: MCP state is never cached) — a cached token is how a turn
// ends up presenting a credential the user revoked two turns ago. Connections are made
// per turn by the layer above; a remote server that is not authorized surfaces in that
// turn's catalog as "not authorized — ^p, then a" and never as a hang (spec §10).
configureOAuthCallback({ port: PORT });
try {
  const remotes = Object.entries(loadRegistry().servers).filter(([, s]) => s.url);
  if (remotes.length > 0) {
    const authorized = remotes.filter(([name]) => authStatus(name).authorized).map(([n]) => n);
    console.log(
      `${remotes.length} remote MCP server(s); ${authorized.length} authorized` +
        (authorized.length > 0 ? `: ${authorized.join(", ")}` : "") +
        ` — OAuth callback ${callbackUrl()}`,
    );
  }
} catch (error) {
  // Best-effort, like the registry count above: an unreadable token store must not
  // stop the server from starting. The turn that needs the server will say so itself.
  console.log(`could not read MCP authorization state: ${error}`);
}

// T7.3 — the MCP manager, the per-session grant, and the two program verbs.
//
// The starter is REBUILT rather than edited above, the same append-only discipline as
// every block before it: identical wiring, plus `mcp()` and `mcpStatus()` merged into
// the `extend` seam and named in `granted` so `prompt/assemble.ts` includes
// `prompt/mcp-status.md`. Both halves are required and neither is sufficient (spec §6)
// — without the bridge the prompt documents a verb that rejects at runtime, and
// without the grant the model is never told the verbs exist and will not reach for
// them. Granted at every tier, like `fetch`/`artifact` and unlike `workflow`: a
// subagent is doing part of the same granted work, and refusing it the servers its
// spawner had would fail every delegated MCP task at the first call (spec §7).
//
// `bindTurnGrant` is the line that makes that inheritance real, and it is not
// decoration. `agents/subagent.ts` copies `ctx.mcpGrant` into the child at spawn, so
// a turn whose ctx never had one hands its subagents nothing; setting a plain array
// instead would fix that and freeze the grant for the whole turn, so a revocation
// would keep working until the turn ended. It installs a LIVE READ of the
// activations instead: every access re-reads them — which is what makes a revoked
// grant visible to the very next `mcpStatus()` call — while the one access that
// matters for inheritance, the spawn, copies the value out as the snapshot spec §7
// describes (`mcp/manager.ts`).
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
      "mcp",
      "mcpStatus",
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
    ...createMcpHostFns(bindTurnGrant(turnCtx)),
  }),
});

// Nothing else is constructed for MCP, and that is deliberate: the registry is a file
// read fresh on every use, grants live in it, and a connection is a live child process
// that only a call creates (plan §6.13). The process manager is touched here purely so
// its subprocesses are reported at shutdown beside the count `killAllMcpServers` gives
// — the kill itself is already hung on process `exit` by the T7.1 block above, which is
// the path every exit converges on.
process.on("exit", () => {
  const open = mcpManager().statuses().length;
  if (open > 0) console.log(`shutdown: ${open} MCP connection(s) were open`);
});

// T7.4 — symbol navigation. One rebuilt starter and one boot report.
//
// The starter is REBUILT rather than edited above, the same append-only discipline as
// every block before it: identical wiring, plus `lsp()` merged into the `extend` seam,
// `"lsp"` named in `granted`, and one new thing no earlier block needed — an
// `assemble` override.
//
// WHY THE OVERRIDE. `prompt/lsp.md` is gated on TWO facts: the verb is bridged
// (`granted`) and the backend is actually installed (`PromptInput.lsp`). The first is
// fixed for the life of a starter; the second is a fact about the machine that can
// change while the server runs — a user who installs the backend mid-session should
// get symbol navigation on their next turn, not after a restart. `TurnDeps.assemble`
// is the seam the runner already calls once per turn, so resolving availability there
// is what keeps the two in step. It is a filesystem stat, never a spawn (`lsp/lsp.ts`),
// so the laziness spec §10 requires survives being asked every turn.
//
// WHY THE BRIDGE IS UNCONDITIONAL while the prompt section is not. A turn told about a
// verb it cannot call wastes a round, which is the failure `granted` exists to prevent
// — but the reverse here is harmless and useful: with no backend installed the model is
// never told the verbs exist, and the one path that could still reach them (a program
// that guessed) gets a sentence saying the backend is not installed rather than
// "unknown host function". Gating the bridge at boot instead would freeze the answer
// for the life of the process.
//
// Granted at every tier, like `fetch`/`artifact`/`mcp` and unlike `workflow`: a
// subagent handed "rename X across the codebase" needs the same verbs its spawner had,
// and reading symbols is as core to delegated work as `bash` is.
//
// Nothing is constructed, connected or recovered here. The backend is a lazy
// subprocess: the first `lsp.*` call of a turn registers the workspace and wakes the
// daemon, and a turn that never asks about a symbol never pays for a language server.
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
      "mcp",
      "mcpStatus",
      "lsp",
    ],
    // Resolved per turn, for the reason above. Everything else the runner passes in
    // (kind, granted, the workspace note) is forwarded untouched.
    assemble: (input) => assemblePrompt({ ...input, lsp: lspAvailable() }),
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
    ...createMcpHostFns(bindTurnGrant(turnCtx)),
    ...createLspHostFn(turnCtx),
  }),
});

// T8.5 — the Changes rail. NOTHING is constructed, attached or recovered here, and
// that is a property of the design rather than an omission: the working tree IS the
// tip (spec §13), so the rail has no substrate, no cache and no in-memory state to
// rebuild. Its two routes are appended in `app.ts` and read the checkout live.
//
// The one thing it does need is `sessions.base`, and the place that must write it is
// the place a session is CREATED — `server/sessions.ts`, already wired — not this
// file. A base captured at boot, or on a session's first turn, would be a sha taken
// after work had already landed in the tree, which hides exactly the changes the rail
// exists to show. There is deliberately no backfill pass for rows created before this
// task either: stamping today's HEAD onto an old session would silently declare its
// whole diff reviewed.

// The boot report. Not a gate and not a probe — it stats for the binary and says what
// it found, because "the model never mentions lsp" and "the backend is not installed"
// look identical from the outside and this is the only place that can tell them apart
// cheaply. Absence is normal and is not a warning: bough works without it, which is
// exactly what the fallback in `prompt/lsp.md` is for.
{
  const bin = findBackend();
  console.log(
    bin
      ? `lsp backend: ${bin} — symbol navigation enabled (nothing spawns until the first call)`
      : `lsp backend: ${BACKEND_NAME} not installed — symbol navigation off; programs use ` +
        `rg + view + patch. Set ${BIN_ENV_VAR} to point at it.`,
  );
}

// T8.6 — keyword search over transcripts. Two wirings, and the first one is the
// required half.
//
// THE HANDLE IS SWAPPED FOR A SEARCH-SAFE ONE. Indexing runs on the INSERT path —
// `server/sessions.ts` indexes a posted message as it persists it, and the turn runner,
// the branch seeder, the note poster, the subagent launcher and the schedule ticker all
// do the same — which is what keeps the index current with no background job and no
// lazy catch-up that makes the first search of the day slow. The price of being on that
// path is that a broken `messages_fts` becomes a broken `POST /sessions/:id/messages`,
// and losing a user's message to a search-index error is not a trade anyone would take.
// Most of those call sites already guard themselves; this is what makes the guarantee
// hold for the one that does not and for every future one, at the seam, once. Every
// other method delegates untouched, so nothing downstream can tell (`server/search.ts`).
//
// Assigned to `ctx.db` rather than to the module's `db` const on purpose: the raw handle
// is what boot recovery, the mirrors and the shutdown path above already captured, and
// they neither index nor should be reading through a wrapper. Every REQUEST and every
// TURN reads `ctx.db`, which is the whole write path this protects. It lands before the
// listener can serve anything: `Bun.serve` above cannot dispatch until this module
// finishes evaluating, and nothing between here and there awaits.
ctx.db = searchSafeDb(db);

// Second, the messages boot recovery just closed. A turn that died mid-stream never
// reached the finish path that indexes its message (`turn/runner.ts`), so everything the
// supervisor had already said in it would be unsearchable forever — and this is the one
// moment those messages are known, closed and enumerated. Idempotent, like every other
// index write, so a message that was already indexed simply gets the same rows back.
if (orphaned.length > 0) {
  const reindexed = indexRecoveredMessages(ctx.db, orphaned);
  if (reindexed > 0) console.log(`re-indexed ${reindexed} message(s) closed by turn recovery`);
}

console.log(
  "keyword search: GET /search?q= over every transcript — bare words are ANDed, " +
    'quote a phrase as "like this" (FTS, no embeddings); POST /search/reindex rebuilds it',
);

// T10.4 — theming. NOTHING is constructed here, and that is the whole design: a theme is
// a JSON file read on request and served to whoever asks (spec §16, "a theme is pure
// data, no rebuild"). No cache, because the file is the source of truth and a cached
// palette is how a `PUT` lands and the next `GET` denies it; no boot validation, because
// a corrupt file resolves to the default palette rather than to an error
// (`server/theme.ts`). The three routes are appended in `app.ts`.

// T10.1 — the cheap tier: auto session titles, composer ghost text, live activity
// blurbs. THREE lines, and every one of them is fire-and-forget by construction.
//
// This is the wiring plan §8.4 calls a risk — "the cheap tier bills continuously; a
// synchronous failure there stalls turns for a cosmetic feature" — so the shape matters
// more than usual:
//
//   - `ctx.cheap` is the only thing installed on the context. It is OPTIONAL in `AppCtx`
//     on purpose: every reader (`history/compact.ts`'s branch rename, the two watchers
//     below, the ghost route) degrades to doing nothing when it is absent, which is what
//     keeps every test that builds its own ctx hermetic and offline with no stub to
//     remember (plan §7).
//   - All three methods resolve `null` on failure and never reject. That is enforced
//     structurally rather than by convention: they share one primitive, `cheapText`,
//     which has no throwing branch and carries a deadline, so a missing API key, a
//     provider 500 and a connection that never answers are the same non-event
//     (`worker/titles.ts`).
//   - Nothing here is ever awaited by a turn. Titles and blurbs are BUS LISTENERS, so
//     they hang off events that are already published and cost the turn runner nothing;
//     ghost text is its own HTTP request from the composer and touches no turn at all.
//
// The model is read per call from `BOUGH_CHEAP_MODEL` (default `claude-haiku-4-5`),
// never from `ctx.model`: spec §12's two tiers are chosen separately in the picker, and
// a user pinned to Opus for the coding work must not pay Opus rates to name a session.
ctx.cheap = {
  title: (firstMessage) => cheapTitle(firstMessage),
  ghostText: (prefix) => cheapGhost(prefix),
  activity: (recent) => cheapActivity(recent),
};

// Auto titles. Without this line a session created untitled stays untitled forever: the
// title is generated from the FIRST user message, and this is the subscription that sees
// it. The unsubscribes are discarded deliberately — both watchers live for the life of
// the process, and holding a thunk nobody calls would only imply otherwise.
watchTitles(ctx);

// Activity blurbs, and the invariant plan §6.11 names: ONE in-flight blurb per session,
// rounds that land while it is busy are DROPPED rather than queued. The ledger lives in
// the watcher's closure, so it is per-subscription rather than global — which is what
// lets a test run its own watcher over its own bus without inheriting this one's state.
watchActivity(ctx);

console.log(
  `cheap tier: ${cheapModel()} (${CHEAP_MODEL_ENV}) — auto titles, composer ghost text ` +
    "(POST /sessions/:id/ghost), live activity blurbs. Fire-and-forget: every failure is " +
    "silent and none of them can delay a turn.",
);

// T10.2 — skills. One rebuilt starter, and it carries BOTH halves of what a `/name`
// invocation means: the skill's instructions in this turn's prompt, and the MCP
// servers it lists granted to this turn (spec §16 — the invocation IS the grant).
//
// The starter is REBUILT rather than edited above, the same append-only discipline as
// every block before it: identical wiring to T7.4's, plus the two seams below. No new
// host function is bridged and nothing is added to `granted` — a skill is instructions,
// not a verb.
//
// WHY THE STARTER IS BUILT PER START. Every other block installs one starter for the
// life of the process, because everything it wires is fixed for the life of the
// process. Skills are not: which ones apply is a fact about the message that opened
// the turn, and `TurnDeps.assemble` — the only seam that reaches the prompt — receives
// a `PromptInput` with no session on it. So the session id is captured HERE, where the
// starter already has it, and the skills themselves are resolved INSIDE the closure at
// assemble time. Both halves matter: capturing the id makes the resolution possible at
// all, and resolving late is what makes a queued drain (`turn/queue.ts`) load the
// skills the QUEUED message named rather than the ones the previous turn used.
//
// Constructing three tier starters per posted message is object construction with no
// I/O (`hostfn/delegate.ts`); the alternative is a module-level "current turn" holder
// read by one seam and written by another, which is a global that breaks silently the
// day the runner reorders two lines.
//
// WHAT A SUBAGENT INHERITS. A child turn is launched with deps derived from this same
// wiring, so it inherits its spawner's skill bodies along with its MCP grant — which is
// the same rule spec §7 states for grants, and the sane one: a delegate doing part of
// skill-directed work should be reading the same instructions. It does not re-resolve
// from its own task text.
//
// NOTHING IS CACHED, and nothing is constructed at boot. `skills/skills.ts` re-walks
// the two source directories per turn, so a SKILL.md edited on disk takes effect on the
// next message with no restart — the same property the MCP registry has and for the
// same reason. The two routes are appended in `app.ts`.

/**
 * Resolving this turn's skills, with a floor under it.
 *
 * `TurnDeps.assemble` is called OUTSIDE the runner's try/catch (`turn/runner.ts`),
 * so a throw from here would escape `drive` before the turn row exists and leave the
 * pending supervisor message closed by nobody — a session the UI shows as busy
 * forever, which is the one failure the whole turn milestone is about. Reading two
 * directories is not worth that risk: a failure logs and the turn runs without
 * skills, which is exactly what it did before this block existed.
 */
const skillsFor = (sessionId: string): ActiveSkills => {
  try {
    return turnSkills(ctx.db, sessionId);
  } catch (error) {
    console.error(`could not resolve skills for session ${sessionId}:`, error);
    return { skills: [], servers: [], names: [], notes: [] };
  }
};

/**
 * `bindTurnGrant` plus whatever servers this turn's skills asked for.
 *
 * The inherited case is checked BEFORE binding, because that is the only moment the
 * two are distinguishable: a subagent arrives with its spawner's snapshot already on
 * its ctx (`agents/subagent.ts`), and widening it here would let a delegate acquire a
 * server its spawner never had — the one thing `requireGranted` promises cannot happen.
 * Its snapshot already contains the spawner's skill servers, so nothing is lost.
 */
const grantedCtxFor = (turnCtx: TurnCtx): TurnCtx => {
  const inherited = turnCtx.mcpGrant !== undefined;
  const bound = bindTurnGrant(turnCtx);
  if (inherited) return bound;
  return widenGrant(bound, skillsFor(turnCtx.sessionId).servers);
};

const skillAwareStarter = (sessionId: string) =>
  createDelegatingTurnStarter({
    base: {
      survivingJobs: (id) => jobs.runningIds(id),
      granted: [
        ...BASE_HOST_FNS,
        "workflow",
        "schedule",
        "image",
        "fetch",
        "ask",
        "state",
        "artifact",
        "mcp",
        "mcpStatus",
        "lsp",
      ],
      // Resolved per turn, like `lsp` beside it. `notes` carries the skills that were
      // NAMED and could not be loaded: a malformed SKILL.md must not make a `/name`
      // vanish silently, and the model is the only thing in the loop that can tell the
      // user their file is broken (`skills/skills.ts`).
      assemble: (input) => {
        const active = skillsFor(sessionId);
        return assemblePrompt({
          ...input,
          lsp: lspAvailable(),
          skills: active.skills,
          notes: [...(input.notes ?? []), ...active.notes],
        });
      },
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
      ...createMcpHostFns(grantedCtxFor(turnCtx)),
      ...createLspHostFn(turnCtx),
    }),
  });

ctx.startTurn = (appCtx, session, message) => {
  skillAwareStarter(session.id)(appCtx, session, message);
};

// The boot report. Skills are files, so the count is the only thing that distinguishes
// "no skills installed" from "the directory the server is reading is not the one you
// wrote into" — and a malformed SKILL.md is named here rather than discovered later as
// a `/name` that quietly did nothing.
{
  const installed = listSkills();
  const broken = installed.filter((s) => s.error);
  console.log(
    `${installed.length} skill(s): ${
      installed.length > 0 ? installed.map((s) => `/${s.name}`).join(", ") : "(none)"
    } — read from ${defaultSources().map((s) => `${s.source} ${s.dir}`).join(", ")}. ` +
      `Name one in a message to load it; GET /skills lists them.`,
  );
  for (const s of broken) console.log(`skill /${s.name} (${s.dir}) will not load: ${s.error}`);
}
