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
import { openDb } from "../db/db.ts";
import { ClawpatrolGateway, setActiveGateway } from "../net/gateway.ts";
import { bus } from "../bus.ts";
import { createHandler } from "../server/app.ts";
import { recoverOrphanedTurns } from "../supervisor/turns.ts";

const db = openDb();
recoverOrphanedTurns(db, bus);
const gateway = new ClawpatrolGateway();
setActiveGateway(gateway);
await gateway.start();

// No port: the desktop webview binds Deno.serve to the port it opened and loads "/".
Deno.serve(createHandler({ db, bus, gateway }));
