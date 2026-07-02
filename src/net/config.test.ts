import { assertEquals } from "jsr:@std/assert@1";
import { defaultConfig, loadConfig, NetConfig, saveConfig, toPolicy } from "./config.ts";
import { decide } from "./policy.ts";

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
