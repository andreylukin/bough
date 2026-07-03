import { assertEquals } from "jsr:@std/assert@1";
import {
  defaultConfig,
  loadConfig,
  NetConfig,
  resolveConfig,
  saveConfig,
  toPolicy,
} from "./config.ts";
import { decide } from "./policy.ts";
import { Db } from "../db/db.ts";
import type { Session } from "../schema/parts.ts";

async function withDir(fn: (dir: string) => void | Promise<void>) {
  const dir = await Deno.makeTempDir({ prefix: "bough-cfg-" });
  try {
    await fn(dir);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
}

Deno.test("loadConfig seeds + persists the default on first run, then reads it back", async () => {
  await withDir((dir) => {
    const first = loadConfig(dir);
    assertEquals(first.mode, "review");
    assertEquals(first.hostMiss, "hold");
    // second load reads the persisted file (no re-seed surprises)
    assertEquals(loadConfig(dir), first);
  });
});

Deno.test("default rule set: read allowed, write held, off-allowlist host held", () => {
  const pol = toPolicy(defaultConfig());
  assertEquals(
    decide({ host: "api.github.com", method: "GET", path: "/user" }, pol).verdict,
    "allow",
  );
  assertEquals(
    decide({ host: "api.github.com", method: "DELETE", path: "/repos/o/r" }, pol).verdict,
    "hold",
  );
  assertEquals(decide({ host: "evil.example.com", method: "GET", path: "/" }, pol).verdict, "hold");
});

Deno.test("editing the rule set changes verdicts: denyHosts + a hold verb + allow a write", async () => {
  await withDir((dir) => {
    const cfg = saveConfig(
      NetConfig.parse({
        mode: "read_only",
        allowHosts: ["api.github.com"],
        denyHosts: ["blocked.example.com"],
        hostMiss: "deny",
        allowVerbs: ["DELETE /repos/o/r"], // explicitly permit this one write
        holdVerbs: ["GET /secret"], // hold a specific read
      }),
      dir,
    );
    const pol = toPolicy(cfg);
    // explicit deny host wins
    assertEquals(
      decide({ host: "blocked.example.com", method: "GET", path: "/" }, pol).verdict,
      "deny",
    );
    // off-allowlist host fails closed (hostMiss=deny)
    assertEquals(
      decide({ host: "other.example.com", method: "GET", path: "/" }, pol).verdict,
      "deny",
    );
    // per-verb allow overrides read_only's write-deny
    assertEquals(
      decide({ host: "api.github.com", method: "DELETE", path: "/repos/o/r" }, pol).verdict,
      "allow",
    );
    // per-verb hold overrides the default read-allow
    assertEquals(
      decide({ host: "api.github.com", method: "GET", path: "/secret" }, pol).verdict,
      "hold",
    );
    // persisted to disk
    assertEquals(loadConfig(dir).denyHosts, ["blocked.example.com"]);
  });
});

// ---- per-branch resolution ---------------------------------------------------

function treeDb(): Db {
  const db = new Db(":memory:");
  const mk = (id: string, parentId: string | null): Session => ({
    id,
    parentId,
    title: id,
    kind: parentId ? "fork" : "root",
    createdAt: 1,
  });
  db.createSession(mk("root", null));
  db.createSession(mk("child", "root"));
  db.createSession(mk("grandchild", "child"));
  return db;
}

Deno.test("resolveConfig: no rows anywhere falls back to the global rule set", async () => {
  await withDir((dir) => {
    const db = treeDb();
    const { config, source } = resolveConfig(db, "grandchild", dir);
    assertEquals(source, { scope: "global" });
    assertEquals(config, loadConfig(dir));
  });
});

Deno.test("resolveConfig: own row wins; descendants inherit it; ancestors don't", async () => {
  await withDir((dir) => {
    const db = treeDb();
    const cfg = NetConfig.parse({ mode: "read_only", allowHosts: ["api.github.com"] });
    db.setNetPolicy("child", JSON.stringify(cfg));

    const own = resolveConfig(db, "child", dir);
    assertEquals(own.source, { scope: "session", sessionId: "child" });
    assertEquals(own.config.mode, "read_only");

    const inherited = resolveConfig(db, "grandchild", dir);
    assertEquals(inherited.source, { scope: "inherited", sessionId: "child" });
    assertEquals(inherited.config.allowHosts, ["api.github.com"]);

    assertEquals(resolveConfig(db, "root", dir).source, { scope: "global" });
  });
});

Deno.test("resolveConfig: nearest override shadows a farther one; corrupt rows are skipped", async () => {
  await withDir((dir) => {
    const db = treeDb();
    db.setNetPolicy("root", JSON.stringify(NetConfig.parse({ mode: "all" })));
    db.setNetPolicy("child", JSON.stringify(NetConfig.parse({ mode: "read_only" })));
    assertEquals(resolveConfig(db, "grandchild", dir).config.mode, "read_only");

    // A corrupt nearest row falls through to the next ancestor, not to an error.
    db.setNetPolicy("child", "{not json");
    const r = resolveConfig(db, "grandchild", dir);
    assertEquals(r.config.mode, "all");
    assertEquals(r.source, { scope: "inherited", sessionId: "root" });
  });
});

Deno.test("generic-host GraphQL classifies by operation, so verb rules gate it", async () => {
  await withDir((dir) => {
    const cfg = saveConfig(
      NetConfig.parse({
        mode: "all",
        allowHosts: ["api.monarchmoney.com"],
        holdVerbs: ["graphql:mutation"],
      }),
      dir,
    );
    const pol = toPolicy(cfg);
    const q = { host: "api.monarchmoney.com", method: "POST", path: "/graphql", body: JSON.stringify({ query: "query { accounts { id } }" }) };
    const m = { ...q, body: JSON.stringify({ query: "mutation { deleteAccount(id: 1) }" }) };
    assertEquals(decide(q, pol).verdict, "allow");
    assertEquals(decide(q, pol).action.verb, "graphql:query");
    assertEquals(decide(m, pol).verdict, "hold");
    assertEquals(decide(m, pol).action.verb, "graphql:mutation");
  });
});
