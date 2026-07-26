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
import { Bus } from "../bus.ts";
import { openDb } from "../db/db.ts";
import { jobs } from "../hostfn/jobs.ts";
import { dbPath } from "../paths.ts";
import { createTurnStarter } from "../turn/runner.ts";
import { recoverOrphanedTurns } from "../turn/state.ts";
import type { AppCtx } from "../types.ts";
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
