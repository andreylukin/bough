import { assert, assertEquals } from "jsr:@std/assert@1";
import { join } from "node:path";
import { validateNet } from "./validate.ts";

async function withDir(fn: (dir: string) => Promise<void>) {
  const dir = await Deno.makeTempDir({ prefix: "bough-validate-" });
  try {
    await fn(dir);
  } finally {
    await Deno.remove(dir, { recursive: true }).catch(() => {});
  }
}

Deno.test("validate: clean dir (no policy, empty library) is ok", async () => {
  await withDir(async (dir) => {
    const r = await validateNet(dir);
    assertEquals(r.ok, true);
    assert(r.lines.some((l) => l.includes("not found")));
    assert(r.lines.some((l) => l === "plugin library: empty"));
  });
});

Deno.test("validate: bad rule condition and broken plugin both error", async () => {
  await withDir(async (dir) => {
    await Deno.writeTextFile(
      join(dir, "policy.json"),
      JSON.stringify({
        mode: "review",
        rules: [{ name: "broken", condition: "http.method ==", verdict: "deny" }],
      }),
    );
    await Deno.mkdir(join(dir, "plugins"));
    await Deno.writeTextFile(
      join(dir, "plugins", "nofix.ts"),
      `export const meta = { name: "nofix", hosts: ["a.example"] };
export const ops = [{ match: "GET *", kind: "read" }];`,
    );
    const r = await validateNet(dir);
    assertEquals(r.ok, false);
    assert(r.lines.some((l) => l.startsWith("error: policy.json")));
    assert(r.lines.some((l) => l.startsWith("error: plugin nofix.ts")));
  });
});

Deno.test("validate: inert activations and dead-end approver chains warn", async () => {
  await withDir(async (dir) => {
    await Deno.writeTextFile(
      join(dir, "policy.json"),
      JSON.stringify({
        mode: "review",
        plugins: [{ name: "ghost" }],
        rules: [
          {
            name: "needs-gate",
            condition: "http.method == 'DELETE'",
            approve: ["plugin:exa-like"],
          },
        ],
      }),
    );
    await Deno.mkdir(join(dir, "plugins"));
    await Deno.writeTextFile(
      join(dir, "plugins", "exa-like.ts"),
      `export const meta = { name: "exa-like", hosts: ["api.example"] };
export const ops = [{ match: "GET *", kind: "read" }];
export const fixtures = [{ req: { method: "GET", path: "/x" }, expect: { kind: "read" } }];`,
    );
    const r = await validateNet(dir);
    assertEquals(r.ok, true); // warnings don't fail the run
    assert(r.lines.some((l) => l.includes(`activation "ghost"`)));
    assert(r.lines.some((l) => l.includes("has no gate()")));
  });
});
