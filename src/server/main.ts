/**
 * Entry point. Opens the DB, wires the app-wide bus, and serves on :4321. This is the
 * process the `dev` task runs; tests build their own ctx + handler instead.
 */
import { openDb } from "../db/db.ts";
import { ClawpatrolGateway, setActiveGateway } from "../net/gateway.ts";
import { bus } from "../bus.ts";
import { createHandler } from "./app.ts";
import { recoverOrphanedTurns } from "../supervisor/turns.ts";

const PORT = Number(Deno.env.get("BOUGH_PORT") ?? 4321);

const db = openDb();
// Any turn left `running` by a previous process crashed mid-flight — orphan it and
// finish its pending message so the UI doesn't show a turn stuck forever.
const orphaned = recoverOrphanedTurns(db, bus);
if (orphaned > 0) console.log(`recovered ${orphaned} orphaned turn(s)`);
// Claw Patrol is the egress firewall (opt-in via BOUGH_CLAWPATROL=1): bough routes
// sandboxed commands through it (its own gateway, or an existing Clawpatrol.app).
const gateway = new ClawpatrolGateway();
setActiveGateway(gateway);
await gateway.start();
globalThis.addEventListener("unload", () => void gateway.stop());
const password = Deno.env.get("BOUGH_PASSWORD");
if (password) console.log("auth: password required (BOUGH_PASSWORD is set)");
const handler = createHandler({ db, bus, gateway, password });

// Deno.serve defaults to 0.0.0.0 — only take the LAN-visible bind when a password
// guards it. BOUGH_HOST overrides either way.
const hostname = Deno.env.get("BOUGH_HOST") ?? (password ? "0.0.0.0" : "127.0.0.1");
Deno.serve({
  port: PORT,
  hostname,
  onListen: ({ hostname, port }) => console.log(`bough listening on ${hostname}:${port}`),
}, handler);
