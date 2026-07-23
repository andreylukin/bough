/**
 * Entry point. Opens the DB, wires the app-wide bus, and serves on :4321. This is the
 * process the `dev` task runs; tests build their own ctx + handler instead.
 */
import { openDb } from "../db/db.ts";
import { ClawpatrolGateway, setActiveGateway } from "../net/gateway.ts";
import { goldenDir, teardownVm } from "../sandbox/vmsession.ts";
import { mcpManager } from "../mcp/manager.ts";
import { bus } from "../bus.ts";
import { createHandler, PURGE_RETENTION_MS } from "./app.ts";
import { recoverOrphanedTurns } from "../supervisor/turns.ts";
import { recoverOrphanedWorkflows } from "../workflow.ts";
import { startScheduleTicker } from "../schedules.ts";
import { watchActivity } from "../worker/activity.ts";
import { workerTitle } from "../supervisor/title.ts";

const PORT = Number(Deno.env.get("BOUGH_PORT") ?? 4321);

const db = openDb();
// Any turn left `running` by a previous process crashed mid-flight — orphan it and
// finish its pending message so the UI doesn't show a turn stuck forever.
const orphaned = recoverOrphanedTurns(db, bus);
if (orphaned > 0) console.log(`recovered ${orphaned} orphaned turn(s)`);
// Workflow runs are in-memory like turns — a row still `running` at boot is stale.
const orphanedWf = recoverOrphanedWorkflows(db);
if (orphanedWf > 0) console.log(`recovered ${orphanedWf} orphaned workflow(s)`);
// Same idea for the net gate: no hold survives a restart, so a `pending` row at
// startup is an orphan whose approval card would otherwise haunt every session.
const swept = db.expirePendingNetEvents("expired — server restarted before approval");
if (swept > 0) console.log(`swept ${swept} orphaned pending net request(s)`);
// Long-term purge: archive is a soft delete with a grace window; sessions archived
// longer than the retention period are hard-removed on boot (and via `bough purge`).
const purged = db.purgeArchivedBefore(Date.now() - PURGE_RETENTION_MS);
if (purged > 0) console.log(`purged ${purged} session(s) archived over 30 days ago`);
// Sandbox backend: bash runs inside a per-session smolvm VM whenever the golden
// rootfs is present (built by scripts/guest-image/build-golden.sh). Explicit
// BOUGH_SANDBOX_VM wins; without a golden, sandboxed bash runs UNSANDBOXED on the
// host (surfaced loudly) until one is built.
if (!Deno.env.get("BOUGH_SANDBOX_VM")) {
  try {
    Deno.statSync(goldenDir());
    Deno.env.set("BOUGH_SANDBOX_VM", "1");
    console.log(`sandbox: VM backend (golden ${goldenDir()})`);
  } catch {
    console.warn(
      `sandbox: no golden rootfs at ${goldenDir()} — sandboxed bash runs UNSANDBOXED; ` +
        `build one with scripts/guest-image/build-golden.sh`,
    );
  }
}
// Claw Patrol is bough's native egress firewall (opt-in via BOUGH_CLAWPATROL=1): it
// runs an in-process intercepting proxy and routes sandboxed commands through it.
const gateway = new ClawpatrolGateway({ db, bus });
setActiveGateway(gateway);
await gateway.start();
// A session's VM persists across its turns (unlike the turn-scoped proxy) and is
// torn down when the session is archived — covers both root sessions (app.ts) and
// subagents (subagent.ts), which both publish session.archived. Idempotent no-op
// when the session never booted a VM.
bus.subscribe((e) => {
  if (e.type === "session.archived" && e.sessionId) void teardownVm(e.sessionId);
});
globalThis.addEventListener("unload", () => void gateway.stop());
// MCP server children (mcp/manager.ts) get an orderly SIGTERM on shutdown.
globalThis.addEventListener("unload", () => void mcpManager().dropAll());
// Live-map blurbs: the local worker narrates each session's current program round
// as ephemeral session.activity events (production wiring only — tests stay hermetic).
watchActivity(bus);
const password = Deno.env.get("BOUGH_PASSWORD");
if (password) console.log("auth: password required (BOUGH_PASSWORD is set)");
const handler = createHandler({
  db,
  bus,
  gateway,
  gate: gateway.gate,
  password,
  retitler: workerTitle,
});
// Recurring runs: every ~30s fire each enabled schedule whose next_run_at has
// passed — one fresh session + turn per fire; a downtime backlog fires once
// (schedules.ts catch-up semantics). Sessions arrive pre-titled from the
// schedule, so no titler is needed here.
startScheduleTicker({ db, bus });

// Deno.serve defaults to 0.0.0.0 — only take the LAN-visible bind when a password
// guards it. BOUGH_HOST overrides either way.
const hostname = Deno.env.get("BOUGH_HOST") ?? (password ? "0.0.0.0" : "127.0.0.1");
Deno.serve({
  port: PORT,
  hostname,
  onListen: ({ hostname, port }) => console.log(`bough listening on ${hostname}:${port}`),
}, handler);
