import { assert, assertEquals } from "jsr:@std/assert@1";
import { join } from "node:path";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { NetStore } from "../db/net.ts";
import { createGate } from "../net/gate.ts";
import { serveWeb } from "./static.ts";
import { createHandler, type AppCtx } from "./app.ts";

async function fakeDist(): Promise<string> {
  const dir = await Deno.makeTempDir();
  await Deno.writeTextFile(join(dir, "index.html"), "<!doctype html><title>bough</title>");
  await Deno.mkdir(join(dir, "assets"));
  await Deno.writeTextFile(join(dir, "assets", "app.js"), "console.log('hi')");
  return dir;
}

function ctx(webDir: string): AppCtx {
  const bus = new Bus();
  const netStore = new NetStore(":memory:");
  return { db: new Db(":memory:"), bus, netStore, gate: createGate({ netStore, bus }), webDir };
}

const get = (path: string) => new Request("http://x" + path);

Deno.test("serveWeb: serves an existing file with the right content-type + cache", async () => {
  const dir = await fakeDist();
  const res = await serveWeb(get("/assets/app.js"), dir);
  assertEquals(res.status, 200);
  assertEquals(res.headers.get("content-type"), "text/javascript; charset=utf-8");
  assert(res.headers.get("cache-control")!.includes("immutable")); // hashed asset
  assertEquals(await res.text(), "console.log('hi')");
  await Deno.remove(dir, { recursive: true });
});

Deno.test("serveWeb: SPA fallback returns index.html for unknown routes", async () => {
  const dir = await fakeDist();
  const res = await serveWeb(get("/sessions/abc/turn/3"), dir);
  assertEquals(res.status, 200);
  assertEquals(res.headers.get("content-type"), "text/html; charset=utf-8");
  assertEquals(res.headers.get("cache-control"), "no-cache");
  assert((await res.text()).includes("<title>bough</title>"));
  await Deno.remove(dir, { recursive: true });
});

Deno.test("serveWeb: path traversal is blocked", async () => {
  const dir = await fakeDist();
  const res = await serveWeb(get("/../../../../etc/passwd"), dir);
  // Either forbidden, or normalized to the SPA fallback — never the real file.
  assert(res.status === 403 || res.headers.get("content-type") === "text/html; charset=utf-8");
  await Deno.remove(dir, { recursive: true });
});

Deno.test("serveWeb: placeholder page for a navigation when no build is present", async () => {
  const dir = await Deno.makeTempDir(); // empty, no index.html
  const res = await serveWeb(get("/"), dir);
  assertEquals(res.status, 200);
  assertEquals(res.headers.get("content-type"), "text/html; charset=utf-8");
  assert((await res.text()).includes("npm run build"));
  await Deno.remove(dir, { recursive: true });
});

Deno.test("serveWeb: missing asset (has extension) is a real 404, not HTML", async () => {
  const dir = await fakeDist();
  const res = await serveWeb(get("/assets/does-not-exist.js"), dir);
  assertEquals(res.status, 404);
  assertEquals(res.headers.get("content-type"), "application/json");
  await Deno.remove(dir, { recursive: true });
});

Deno.test("createHandler: API routes win over static; other GETs hit the SPA", async () => {
  const dir = await fakeDist();
  const c = ctx(dir);
  const h = createHandler(c);

  // API path still returns JSON, not index.html.
  const api = await h(get("/sessions"));
  assertEquals(api.headers.get("content-type"), "application/json");
  assertEquals(await api.json(), []);

  // Root + unknown client route serve the SPA shell.
  const root = await h(get("/"));
  assertEquals(root.headers.get("content-type"), "text/html; charset=utf-8");

  c.db.close();
  await Deno.remove(dir, { recursive: true });
});
