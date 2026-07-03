import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { Db } from "../db/db.ts";
import { ExtensionHost } from "./extensions.ts";
import { createGate } from "./gate.ts";
import { decide, policy, type Request } from "./policy.ts";

const ghGet: Request = { host: "api.github.com", method: "GET", path: "/user" };
const openPolicy = policy(); // host-open, read-only default → GET allows

function staticDecision(req: Request) {
  return decide(req, openPolicy);
}

Deno.test("extension chain: first verdict wins, undefined falls through, errors skip", async () => {
  const host = new ExtensionHost(new Db(":memory:"), { timeoutMs: 200 });
  const seen: string[] = [];
  host.register({
    name: "passer",
    gate: () => {
      seen.push("passer");
      return undefined;
    },
  });
  host.register({
    name: "thrower",
    gate: () => {
      seen.push("thrower");
      throw new Error("boom");
    },
  });
  host.register({ name: "denier", gate: () => ({ verdict: "deny", reason: "nope" }) });
  host.register({ name: "never", gate: () => "allow" });

  const out = await host.gate(ghGet, staticDecision(ghGet), "s1");
  assertEquals(seen, ["passer", "thrower"]);
  assertEquals(out, { verdict: "deny", reason: "nope", by: "denier" });
});

Deno.test("extension chain: a hung guard times out and falls through", async () => {
  const host = new ExtensionHost(new Db(":memory:"), { timeoutMs: 30 });
  host.register({ name: "hang", gate: () => new Promise(() => {}) });
  host.register({ name: "after", gate: () => "allow" });
  const out = await host.gate(ghGet, staticDecision(ghGet));
  assertEquals(out?.by, "after");
});

Deno.test("extension state: persists in the DB across host instances", async () => {
  const db = new Db(":memory:");
  const h1 = new ExtensionHost(db);
  h1.register({
    name: "counter",
    gate: (_req, ctx) => {
      ctx.state.set("mark", { n: 42 });
      return undefined;
    },
  });
  await h1.gate(ghGet, staticDecision(ghGet));

  let read: unknown;
  const h2 = new ExtensionHost(db); // fresh host, same DB — a restart
  h2.register({
    name: "counter",
    gate: (_req, ctx) => {
      read = ctx.state.get("mark");
      return undefined;
    },
  });
  await h2.gate(ghGet, staticDecision(ghGet));
  assertEquals(read, { n: 42 });
});

Deno.test("extension loading: dir modules load, broken files are listed not fatal", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-ext-" });
  try {
    await Deno.writeTextFile(
      join(dir, "deny-evil.ts"),
      `export const name = "deny-evil";
       export function gate(req) {
         if (req.host === "evil.example.com") return { verdict: "deny", reason: "evil" };
       }`,
    );
    await Deno.writeTextFile(join(dir, "broken.ts"), "export const gate = 5;");
    const host = new ExtensionHost(new Db(":memory:"));
    await host.load(dir);

    const infos = host.list();
    assertEquals(infos.map((i) => i.name).sort(), ["broken.ts", "deny-evil"]);
    assertStringIncludes(infos.find((i) => i.error)!.error!, "gate(req, ctx)");

    const evil: Request = { host: "evil.example.com", method: "GET", path: "/" };
    assertEquals((await host.gate(evil, staticDecision(evil)))?.verdict, "deny");
    assertEquals(await host.gate(ghGet, staticDecision(ghGet)), undefined);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("gate integration: a guard hold parks the request until resolved", async () => {
  const bus = new Bus();
  const db = new Db(":memory:");
  const host = new ExtensionHost(db);
  host.register({ name: "suspicious", gate: () => ({ verdict: "hold", reason: "check me" }) });
  const gate = createGate({ db, bus, policy: openPolicy, extensions: host });

  const pending = gate.gate(ghGet, { sessionId: "s1" }); // static allow, guard holds
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(gate.pending, 1);
  const id = db.recentNetEvents("s1")[0].id;
  gate.resolveHold(id, true);
  assertEquals((await pending).verdict, "allow");
});

// The shipped example, end to end: branch created on the wire → merge allowed;
// unknown branch → held. The gh API is stubbed via fetchImpl.
Deno.test("gh-merge-guard example: merge allowed only for a session-created branch", async () => {
  const db = new Db(":memory:");
  const fakeGh = ((input: RequestInfo | URL) => {
    const url = String(input);
    const m = url.match(/\/pulls\/(\d+)$/);
    const head = m?.[1] === "7" ? "feat-x" : "someone-elses-branch";
    return Promise.resolve(
      new Response(JSON.stringify({ state: "open", head: { ref: head } }), { status: 200 }),
    );
  }) as typeof fetch;
  const host = new ExtensionHost(db, { fetchImpl: fakeGh });
  await host.load(new URL("../../examples/net-extensions", import.meta.url).pathname);
  assertEquals(host.list().filter((i) => i.error), []);

  const createBranch: Request = {
    host: "api.github.com",
    method: "POST",
    path: "/repos/o/r/git/refs",
    body: JSON.stringify({ ref: "refs/heads/feat-x", sha: "abc" }),
  };
  const mergeMine: Request = { host: "api.github.com", method: "PUT", path: "/repos/o/r/pulls/7/merge" };
  const mergeOther: Request = { host: "api.github.com", method: "PUT", path: "/repos/o/r/pulls/9/merge" };

  // Branch creation passes through (static policy's call) but is remembered.
  assertEquals(await host.gate(createBranch, staticDecision(createBranch), "s1"), undefined);

  const mine = await host.gate(mergeMine, staticDecision(mergeMine), "s1");
  assertEquals(mine?.verdict, "allow");
  assertStringIncludes(mine!.reason, "created by this session");

  const other = await host.gate(mergeOther, staticDecision(mergeOther), "s1");
  assertEquals(other?.verdict, "hold");
  assertStringIncludes(other!.reason, "did not create");

  // A different session merging the same PR is also held — the fact is per-branch.
  const stranger = await host.gate(mergeMine, staticDecision(mergeMine), "s2");
  assertEquals(stranger?.verdict, "hold");
});
