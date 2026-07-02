import { assert, assertEquals } from "jsr:@std/assert@1";
import { Db } from "../db/db.ts";
import { Bus } from "../bus.ts";
import { NetStore } from "../db/net.ts";
import { createGate } from "../net/gate.ts";
import { policy } from "../net/policy.ts";
import { createHandler, type AppCtx } from "./app.ts";
import type { BoughEvent, NetRequest } from "../schema/parts.ts";

function ctx(opts: { netDir?: string; holdVerbs?: string[] } = {}): AppCtx {
  const bus = new Bus();
  const netStore = new NetStore(":memory:");
  const pol = opts.holdVerbs ? policy({ holdVerbs: new Set(opts.holdVerbs) }) : policy();
  return {
    db: new Db(":memory:"),
    bus,
    netStore,
    gate: createGate({ netStore, bus, policy: pol }),
    netDir: opts.netDir,
  };
}

const req = (method: string, path: string, body?: unknown) =>
  new Request("http://x" + path, {
    method,
    headers: body ? { "content-type": "application/json" } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });

Deno.test("GET /net/requests is empty then reflects gated requests", async () => {
  const c = ctx();
  const h = createHandler(c);
  assertEquals(await (await h(req("GET", "/net/requests"))).json(), []);

  await c.gate.gate({ host: "api.github.com", method: "GET", path: "/user" }, { sessionId: "s1" });
  const rows = await (await h(req("GET", "/net/requests?sessionId=s1"))).json() as NetRequest[];
  assertEquals(rows.length, 1);
  assertEquals(rows[0].verdict, "allowed");
  // filtered out for a different session
  assertEquals((await (await h(req("GET", "/net/requests?sessionId=other"))).json() as unknown[]).length, 0);
  c.db.close();
});

Deno.test("GET /net/bundles lists github; detail has fixtures; unknown → 404", async () => {
  const dir = await Deno.makeTempDir(); // isolate installed-state from the real home dir
  const c = ctx({ netDir: dir });
  const h = createHandler(c);
  const list = await (await h(req("GET", "/net/bundles"))).json() as Array<
    { name: string; installed: boolean; params: unknown[] }
  >;
  const gh = list.find((b) => b.name === "github");
  assert(gh, "github bundle listed");
  assertEquals(gh!.installed, false);
  assert(Array.isArray(gh!.params));

  const detail = await (await h(req("GET", "/net/bundles/github"))).json() as { fixtures: unknown[] };
  assertEquals(detail.fixtures.length, 4);

  assertEquals((await h(req("GET", "/net/bundles/nope"))).status, 404);
  c.db.close();
  await Deno.remove(dir, { recursive: true });
});

Deno.test("POST /net/bundles/github/install validates + persists; bad params → 400", async () => {
  const dir = await Deno.makeTempDir();
  const c = ctx({ netDir: dir });
  const h = createHandler(c);

  const ok = await h(req("POST", "/net/bundles/github/install", { params: {} }));
  assertEquals(ok.status, 200);
  const body = await ok.json() as { ok: boolean; hcl: string };
  assertEquals(body.ok, true);
  assert(body.hcl.includes("api.github.com"));
  // now shows installed
  const list = await (await h(req("GET", "/net/bundles"))).json() as Array<{ name: string; installed: boolean }>;
  assertEquals(list.find((b) => b.name === "github")?.installed, true);

  const bad = await h(req("POST", "/net/bundles/github/install", { params: { host: 123 } }));
  assertEquals(bad.status, 400);

  c.db.close();
  await Deno.remove(dir, { recursive: true });
});

// ---- hold-and-ask over HTTP + SSE ------------------------------------------

Deno.test("hold flow: pending emitted on /events, approved via POST, gate resolves allow", async () => {
  const c = ctx({ holdVerbs: ["GET /user"] });
  const server = Deno.serve({ port: 0, onListen() {} }, createHandler(c));
  const { port } = server.addr as Deno.NetAddr;
  const origin = `http://127.0.0.1:${port}`;

  const evRes = await fetch(`${origin}/events`);
  const reader = evRes.body!.getReader();
  const dec = new TextDecoder();
  const frames: BoughEvent[] = [];
  const nextNet = () =>
    (async () => {
      let buf = "";
      while (true) {
        const before = frames.filter((f) => f.type === "net.request").length;
        const { value, done } = await reader.read();
        if (done) return;
        buf += dec.decode(value, { stream: true });
        let i;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const frame = buf.slice(0, i);
          buf = buf.slice(i + 2);
          const m = frame.match(/^data: (.+)$/m);
          if (m) frames.push(JSON.parse(m[1]) as BoughEvent);
        }
        if (frames.filter((f) => f.type === "net.request").length > before) return;
      }
    })();

  // The gateway addon parks on a held request.
  const gated = c.gate.gate({ host: "api.github.com", method: "GET", path: "/user" }, {
    sessionId: "s1",
    requestedBy: "worker",
  });

  await nextNet();
  const pending = frames.find((f) => f.type === "net.request")!.data as NetRequest;
  assertEquals(pending.verdict, "pending");

  // Driver approves.
  const res = await fetch(`${origin}/net/requests/${pending.id}/allow`, { method: "POST" });
  assertEquals(res.status, 200);

  await nextNet();
  const resolved = (await gated).verdict;
  assertEquals(resolved, "allow");
  const final = frames.filter((f) => f.type === "net.request").at(-1)!.data as NetRequest;
  assertEquals(final.id, pending.id);
  assertEquals(final.verdict, "allowed");

  // denying an already-resolved id → 404
  const again = await fetch(`${origin}/net/requests/${pending.id}/deny`, { method: "POST" });
  assertEquals(again.status, 404);

  reader.cancel();
  await server.shutdown();
  c.db.close();
});
