import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { join } from "node:path";
import {
  buildClassifier,
  type PluginGuard,
  PluginHost,
  type PluginSpec,
  renderModule,
  runFixtures,
  runGuards,
  specFromRequests,
  ttlToExpires,
} from "./plugins.ts";
import { decide, policy, type Request } from "./policy.ts";
import { createGate } from "./gate.ts";
import { Bus } from "../bus.ts";
import { Db } from "../db/db.ts";

const stripeSpec: PluginSpec = {
  meta: {
    name: "stripe",
    description: "Gate destructive Stripe ops.",
    hosts: ["api.stripe.com"],
  },
  ops: [
    { match: "GET *", kind: "read" },
    { match: "POST /v1/refunds*", kind: "write", verb: "stripe:refund" },
    { match: "DELETE /v1/customers/*", kind: "write", verb: "stripe:delete-customer" },
    { match: "POST *", kind: "write" },
  ],
  fixtures: [
    { req: { method: "GET", path: "/v1/charges" }, expect: { kind: "read" } },
    {
      req: { method: "POST", path: "/v1/refunds" },
      expect: { kind: "write", verb: "stripe:refund" },
    },
    {
      req: { method: "DELETE", path: "/v1/customers/cus_9" },
      expect: { verb: "stripe:delete-customer" },
    },
  ],
};

function stripeClassifier() {
  return buildClassifier(stripeSpec.meta.name, stripeSpec.meta.hosts, stripeSpec.ops);
}

Deno.test("ops matcher: first match wins, globs, verb default, query stripped", () => {
  const c = stripeClassifier();
  const get = c.classify({ host: "api.stripe.com", method: "GET", path: "/v1/charges?limit=3" })!;
  assertEquals(get, { service: "stripe", verb: "GET /v1/charges", kind: "read" });

  const refund = c.classify({ host: "api.stripe.com", method: "POST", path: "/v1/refunds/re_1" })!;
  assertEquals(refund.verb, "stripe:refund"); // specific row beats the POST catch-all

  const create = c.classify({ host: "api.stripe.com", method: "post", path: "/v1/customers" })!;
  assertEquals(create, { service: "stripe", verb: "POST /v1/customers", kind: "write" });
});

Deno.test("ops matcher: no matching row fails closed as unknown, service kept", () => {
  const c = stripeClassifier();
  const patch = c.classify({ host: "api.stripe.com", method: "PATCH", path: "/v1/plans/p1" })!;
  assertEquals(patch, { service: "stripe", verb: "PATCH /v1/plans/p1", kind: "unknown" });
});

Deno.test("custom classify: runs before ops, throw yields unknown (not fall-through)", () => {
  const custom = buildClassifier("x", ["h.example"], [{ match: "GET *", kind: "read" }], (req) => {
    if (req.path === "/boom") throw new Error("bug");
    if (req.path === "/special") return { service: "x", verb: "x:special", kind: "write" };
    return undefined; // fall to the ops table
  });
  assertEquals(
    custom.classify({ host: "h.example", method: "GET", path: "/special" })!.verb,
    "x:special",
  );
  assertEquals(custom.classify({ host: "h.example", method: "GET", path: "/ok" })!.kind, "read");
  assertEquals(
    custom.classify({ host: "h.example", method: "GET", path: "/boom" })!.kind,
    "unknown",
  );
});

Deno.test("decide: plugin verbs gate destructive ops while reads flow", () => {
  const plugins = [stripeClassifier()];
  const pol = policy({ mode: "read_only", holdVerbs: new Set(["stripe:refund"]) });

  const read: Request = { host: "api.stripe.com", method: "GET", path: "/v1/charges" };
  assertEquals(decide(read, pol, plugins).verdict, "allow");

  const refund: Request = { host: "api.stripe.com", method: "POST", path: "/v1/refunds" };
  assertEquals(decide(refund, pol, plugins).verdict, "hold"); // explicit holdVerbs override

  const del: Request = { host: "api.stripe.com", method: "DELETE", path: "/v1/customers/c" };
  assertEquals(decide(del, pol, plugins).verdict, "deny"); // write in read_only

  const patch: Request = { host: "api.stripe.com", method: "PATCH", path: "/v1/plans/p" };
  assertEquals(decide(patch, pol, plugins).verdict, "deny"); // unknown fails closed
});

Deno.test("decide: plugins outrank built-ins for their hosts, others untouched", () => {
  const gh = buildClassifier("gh-tight", ["api.github.com"], [
    { match: "GET *", kind: "read" },
    { match: "PUT /repos/*/merge", kind: "write", verb: "gh:merge" },
  ]);
  const pol = policy({ mode: "read_only" });
  const merge: Request = {
    host: "api.github.com",
    method: "PUT",
    path: "/repos/o/r/pulls/1/merge",
  };
  // built-in github classifier would say "PUT /repos/..."; the plugin renames it
  assertEquals(decide(merge, pol, [gh]).action.verb, "gh:merge");
  // a non-plugin host still hits the built-in chain
  const aws: Request = {
    host: "ec2.amazonaws.com",
    method: "POST",
    path: "/",
    body: "Action=DescribeInstances",
  };
  assertEquals(decide(aws, pol, [gh]).action.verb, "DescribeInstances");
});

Deno.test("runFixtures: reports kind/verb mismatches by name", () => {
  const failures = runFixtures(stripeClassifier(), ["api.stripe.com"], [
    { name: "bad", req: { method: "GET", path: "/v1/charges" }, expect: { kind: "write" } },
    { req: { method: "POST", path: "/v1/refunds" }, expect: { verb: "stripe:refund" } },
  ]);
  assertEquals(failures.length, 1);
  assertStringIncludes(failures[0], "bad: kind read, expected write");
});

Deno.test("ttlToExpires: shorthand → ISO; junk rejected", () => {
  assertEquals(ttlToExpires("2h", 0), new Date(7_200_000).toISOString());
  assertEquals(ttlToExpires("7d", 0), new Date(604_800_000).toISOString());
  let threw = false;
  try {
    ttlToExpires("soon");
  } catch {
    threw = true;
  }
  assertEquals(threw, true);
});

Deno.test("loader: valid module gates, broken + fixture-failing files are listed not fatal", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-plug-" });
  try {
    await Deno.writeTextFile(join(dir, "stripe.ts"), renderModule(stripeSpec));
    await Deno.writeTextFile(
      join(dir, "no-fixtures.ts"),
      `export const meta = { name: "nofix", hosts: ["a.example"] };
       export const ops = [{ match: "GET *", kind: "read" }];`,
    );
    await Deno.writeTextFile(
      join(dir, "wrong.ts"),
      `export const meta = { name: "wrong", hosts: ["b.example"] };
       export const ops = [{ match: "GET *", kind: "read" }];
       export const fixtures = [{ req: { method: "GET", path: "/x" }, expect: { kind: "write" } }];`,
    );
    const host = new PluginHost(dir);
    await host.load();

    const infos = host.list();
    assertEquals(infos.map((i) => [i.name, i.status]).sort(), [
      ["no-fixtures.ts", "error"],
      ["stripe", "loaded"],
      ["wrong.ts", "error"],
    ]);
    assertStringIncludes(infos.find((i) => i.name === "no-fixtures.ts")!.error!, "fixture");
    assertStringIncludes(infos.find((i) => i.name === "wrong.ts")!.error!, "fixtures failed");

    // only the healthy plugin gates, and only when an activation names it
    assertEquals(host.activeFor([{ name: "stripe" }]).map((c) => c.name), ["stripe"]);
    assertEquals(host.activeFor([]), []);
    assertEquals(host.activeFor([{ name: "wrong" }, { name: "ghost" }]), []);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("loader: duplicate plugin names — second file errors, first gates", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-plug-dup-" });
  try {
    await Deno.writeTextFile(join(dir, "a-stripe.ts"), renderModule(stripeSpec));
    await Deno.writeTextFile(join(dir, "b-stripe.ts"), renderModule(stripeSpec));
    const host = new PluginHost(dir);
    await host.load();
    assertEquals(host.activeFor([{ name: "stripe" }]).length, 1);
    assertStringIncludes(
      host.list().find((i) => i.status === "error")!.error!,
      "already registered",
    );
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("TTL lives on the activation: same plugin, different expiry per scope", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-plug-ttl-" });
  try {
    await Deno.writeTextFile(join(dir, "stripe.ts"), renderModule(stripeSpec));
    const host = new PluginHost(dir);
    await host.load();

    const now = Date.now();
    const expiring = { name: "stripe", expires: new Date(now + 60_000).toISOString() };
    const openEnded = { name: "stripe" };

    // Before expiry both activations gate; after, only the open-ended one does —
    // expiry can only remove precision, and it never touches the other scope.
    assertEquals(host.activeFor([expiring], now).length, 1);
    assertEquals(host.activeFor([expiring], now + 120_000).length, 0);
    assertEquals(host.activeFor([openEnded], now + 120_000).length, 1);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("install: declarative spec round-trips through a rendered library file", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-plug-install-" });
  try {
    const host = new PluginHost(dir);
    const { path } = await host.install(stripeSpec);
    assertStringIncludes(path, "stripe.ts");
    const src = await Deno.readTextFile(path);
    assertStringIncludes(
      src,
      '{ match: "POST /v1/refunds*", kind: "write", verb: "stripe:refund" },',
    );
    assertEquals(host.list()[0].status, "loaded");

    // reinstall refuses to clobber the (possibly edited) file
    await assertRejects(() => host.install(stripeSpec), Error, "already exists");
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("install: fixture-failing spec throws before touching disk", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-plug-badspec-" });
  try {
    const bad: PluginSpec = {
      ...stripeSpec,
      fixtures: [{ req: { method: "GET", path: "/v1/charges" }, expect: { kind: "write" } }],
    };
    const host = new PluginHost(dir);
    await assertRejects(() => host.install(bad), Error, "fixtures failed");
    assertEquals([...Deno.readDirSync(dir)].length, 0);
  } catch (e) {
    if (!(e instanceof Deno.errors.NotFound)) throw e; // dir never created is fine too
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("scaffold: writes a runnable starter, loads clean, refuses to clobber", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-plug-scaffold-" });
  try {
    const host = new PluginHost(dir);
    const { path } = await host.scaffold("My Stripe Guard!");
    assertStringIncludes(path, "my-stripe-guard.ts");
    assertEquals(host.list().find((i) => i.status === "error"), undefined);
    await assertRejects(() => host.scaffold("my-stripe-guard"), Error, "already exists");
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("gate integration: plugin verb held for approval, resolved by a human", async () => {
  const db = new Db(":memory:");
  const gate = createGate({
    db,
    bus: new Bus(),
    policy: policy({ mode: "read_only", holdVerbs: new Set(["stripe:refund"]) }),
    classifiers: () => [stripeClassifier()],
  });
  const pending = gate.gate(
    { host: "api.stripe.com", method: "POST", path: "/v1/refunds" },
    { sessionId: "s1" },
  );
  await new Promise((r) => setTimeout(r, 0));
  assertEquals(gate.pending, 1);
  const row = db.recentNetEvents("s1")[0];
  assertEquals(row.action, "stripe:refund"); // the rail shows the plugin's verb
  gate.resolveHold(row.id, false);
  assertEquals((await pending).verdict, "deny");
});

Deno.test("plugin hosts skip the allowlist gate while active; hostMiss returns on expiry", () => {
  // Tight allowlist that does NOT include the plugin's host — the exa-panel case.
  const pol = policy({
    mode: "review",
    allowHosts: new Set(["github.com"]),
    hostMiss: "hold",
  });
  const read: Request = { host: "api.stripe.com", method: "GET", path: "/v1/charges" };
  const refund: Request = { host: "api.stripe.com", method: "POST", path: "/v1/refunds" };

  // Active plugin: its table gates — reads flow, writes hold per mode, no host hold.
  const plugins = [stripeClassifier()];
  assertEquals(decide(read, pol, plugins).verdict, "allow");
  const held = decide(refund, pol, plugins);
  assertEquals(held.verdict, "hold");
  assertEquals(held.action.verb, "stripe:refund"); // held by the TABLE, not hostMiss

  // No active plugin (expired/disabled): the hostMiss gate is back in charge.
  const gone = decide(read, pol, []);
  assertEquals(gone.verdict, "hold");
  assertStringIncludes(gone.reason, "not in allowlist");

  // denyHosts still outranks an active plugin.
  const denied = decide(read, policy({ denyHosts: new Set(["api.stripe.com"]) }), plugins);
  assertEquals(denied.verdict, "deny");
});

// ---- contextual gate() ----------------------------------------------------------

Deno.test("gate(): loader accepts it, activeGuardsFor exposes it, host-scoped override wins", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-plug-gate-" });
  try {
    await Deno.writeTextFile(
      join(dir, "aged.ts"),
      `export const meta = { name: "aged", hosts: ["*.s3.amazonaws.com"] };
       export const ops = [
         { match: "GET *", kind: "read" },
         { match: "DELETE *", kind: "write", verb: "aged:delete" },
       ];
       export const fixtures = [
         { req: { method: "DELETE", path: "/k" }, expect: { verb: "aged:delete" } },
       ];
       export async function gate(req, ctx) {
         if (req.method !== "DELETE") return;
         const res = await ctx.fetch("https://" + req.host + req.path, { method: "HEAD" });
         const age = Date.now() - Date.parse(res.headers.get("last-modified"));
         return age < 3_600_000
           ? { verdict: "allow", reason: "young object" }
           : { verdict: "hold", reason: "old object" };
       }`,
    );
    const host = new PluginHost(dir);
    await host.load();
    assertEquals(host.list()[0].hasGate, true);
    const guards = host.activeGuardsFor([{ name: "aged" }]);
    assertEquals(guards.length, 1);

    const freshFetch = ((_i: RequestInfo | URL, _o?: RequestInit) =>
      Promise.resolve(
        new Response(null, { headers: { "last-modified": new Date().toUTCString() } }),
      )) as typeof fetch;
    const staleFetch = ((_i: RequestInfo | URL, _o?: RequestInit) =>
      Promise.resolve(
        new Response(null, { headers: { "last-modified": "Wed, 04 Mar 2020 15:46:30 GMT" } }),
      )) as typeof fetch;

    const del: Request = { host: "b.s3.amazonaws.com", method: "DELETE", path: "/k" };
    const decision = decide(del, policy({ mode: "review" }), host.activeFor([{ name: "aged" }]));
    assertEquals(decision.verdict, "hold"); // static: write in review

    const young = await runGuards(guards, del, decision, "s1", { fetchImpl: freshFetch });
    assertEquals(young, { verdict: "allow", reason: "young object", by: "aged" });
    const old = await runGuards(guards, del, decision, "s1", { fetchImpl: staleFetch });
    assertEquals(old?.verdict, "hold");

    // a GET passes through (gate returns undefined) and other hosts are never consulted
    const get: Request = { host: "b.s3.amazonaws.com", method: "GET", path: "/k" };
    assertEquals(
      await runGuards(guards, get, decision, "s1", { fetchImpl: staleFetch }),
      undefined,
    );
    const other: Request = { host: "api.github.com", method: "DELETE", path: "/k" };
    assertEquals(
      await runGuards(guards, other, decision, "s1", { fetchImpl: staleFetch }),
      undefined,
    );
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
});

Deno.test("gate(): throw and timeout fall through — the static verdict stands", async () => {
  const thrower: PluginGuard = {
    name: "boom",
    hosts: ["h.example"],
    gate: () => {
      throw new Error("bug");
    },
  };
  const hanger: PluginGuard = {
    name: "hang",
    hosts: ["h.example"],
    gate: () => new Promise(() => {}),
  };
  const after: PluginGuard = {
    name: "after",
    hosts: ["h.example"],
    gate: () => ({ verdict: "deny", reason: "after speaks" }),
  };
  const req: Request = { host: "h.example", method: "DELETE", path: "/x" };
  const decision = decide(req, policy({ mode: "review" }));
  const out = await runGuards([thrower, hanger, after], req, decision, undefined, {
    timeoutMs: 30,
  });
  assertEquals(out, { verdict: "deny", reason: "after speaks", by: "after" });
  // all guards broken → undefined → caller keeps the static decision
  assertEquals(
    await runGuards([thrower, hanger], req, decision, undefined, { timeoutMs: 30 }),
    undefined,
  );
});

Deno.test("gate integration: guard override rides through Gate.gate()", async () => {
  const db = new Db(":memory:");
  const guard: PluginGuard = {
    name: "ager",
    hosts: ["api.stripe.com"],
    gate: (req) =>
      req.method === "DELETE" ? { verdict: "allow", reason: "young enough" } : undefined,
  };
  const gate = createGate({
    db,
    bus: new Bus(),
    policy: policy({ mode: "review" }),
    classifiers: () => [stripeClassifier()],
    guards: () => [guard],
  });
  const out = await gate.gate(
    { host: "api.stripe.com", method: "DELETE", path: "/v1/customers/c" },
    { sessionId: "s1" },
  );
  assertEquals(out.verdict, "allow");
  assertEquals(out.reason, "young enough");
  assertEquals(db.recentNetEvents("s1")[0].verdict, "allowed");
});

// ---- group-into-plugin (specFromRequests) --------------------------------------

Deno.test("specFromRequests: distinct actions → op rows; reads vs writes; fixtures valid", () => {
  const spec = specFromRequests([
    { host: "api.stripe.com", verb: "GET", action: "GET /v1/charges" },
    { host: "api.stripe.com", verb: "GET", action: "GET /v1/charges" }, // dup collapses
    { host: "api.stripe.com", verb: "POST", action: "POST /v1/refunds" },
    { host: "api.stripe.com", verb: "DELETE", action: "DELETE /v1/customers/cus_9" },
  ]);
  assertEquals(spec.meta.name, "stripe"); // from the host
  assertEquals(spec.meta.hosts, ["api.stripe.com"]);
  assertEquals(spec.ops, [
    { match: "GET /v1/charges", kind: "read" },
    { match: "POST /v1/refunds", kind: "write" },
    { match: "DELETE /v1/customers/cus_9", kind: "write" },
  ]);
  // the generated fixtures must pass against the generated table (install invariant)
  const c = buildClassifier(spec.meta.name, spec.meta.hosts, spec.ops);
  assertEquals(runFixtures(c, spec.meta.hosts, spec.fixtures), []);
});

Deno.test("specFromRequests: multiple hosts covered; classified verbs fall back to a method catch-all", () => {
  const spec = specFromRequests([
    { host: "sts.us-east-2.amazonaws.com", verb: "POST", action: "GetCallerIdentity" },
    { host: "s3.amazonaws.com", verb: "GET", action: "GET /bucket/key" },
  ]);
  assertEquals(spec.meta.name, "amazonaws");
  assertEquals(spec.meta.hosts, ["sts.us-east-2.amazonaws.com", "s3.amazonaws.com"]);
  // "GetCallerIdentity" isn't "METHOD /path" → method catch-all "POST *"
  assertEquals(spec.ops[0], { match: "POST *", kind: "write" });
  assertEquals(spec.ops[1], { match: "GET /bucket/key", kind: "read" });
  const c = buildClassifier(spec.meta.name, spec.meta.hosts, spec.ops);
  assertEquals(runFixtures(c, spec.meta.hosts, spec.fixtures), []);
});
