/**
 * `deno desktop` entrypoint — packages bough as a native app: the UI renders in the
 * OS webview (WebKit on macOS), the backend runs in this same Deno process, and the whole
 * thing compiles to one binary via `deno task desktop:build`.
 *
 * Contract (Deno 2.9): in a desktop entrypoint, `Deno.serve()` with no address auto-binds
 * to the port the webview opens and loads "/", which our handler serves as the built SPA
 * (see server/static.ts). So this is just the normal server bootstrap minus the fixed
 * port and the console logging. It mirrors server/main.ts on purpose — kept as a separate
 * tiny entrypoint rather than refactoring the shared server/main.ts while other agents
 * are editing it.
 *
 * Experimental: deno desktop is experimental in 2.9. Fallback if it regresses is
 * `deno task dev` + a browser at :4321 (byte-identical handler). See docs/deno-desktop.md.
 */
import { join } from "node:path";
import { homedir } from "node:os";
import { openDb } from "../db/db.ts";
import { ClawpatrolGateway, setActiveGateway } from "../net/gateway.ts";
import { bus } from "../bus.ts";
import { createHandler } from "../server/app.ts";
import { recoverOrphanedTurns } from "../supervisor/turns.ts";

// Finder/`open` launches get launchd's bare environment, not a shell's — the
// launchd service gets its env from scripts/bough sourcing ~/.bough/env, so the
// desktop app has to load it itself (KEY=VALUE lines; existing env wins), and
// put Homebrew on PATH for git/llama-server.
try {
  const envFile = Deno.readTextFileSync(join(homedir(), ".bough", "env"));
  for (const line of envFile.split("\n")) {
    const m = line.match(/^([A-Za-z_][A-Za-z0-9_]*)=(.*)$/);
    if (m && Deno.env.get(m[1]) === undefined) Deno.env.set(m[1], m[2]);
  }
} catch {
  // no ~/.bough/env — run with whatever env we were given
}
Deno.env.set("PATH", `/opt/homebrew/bin:/usr/local/bin:${Deno.env.get("PATH") ?? "/usr/bin:/bin"}`);

const db = openDb();
recoverOrphanedTurns(db, bus);
const gateway = new ClawpatrolGateway({ db, bus });
setActiveGateway(gateway);
await gateway.start();

// No port: the desktop webview binds Deno.serve to the port it opened and loads "/".
Deno.serve(createHandler({ db, bus, gateway, gate: gateway.gate }));
