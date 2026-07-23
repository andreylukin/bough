import { assert, assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { McpManager } from "./manager.ts";
import { saveRegistry } from "./config.ts";
import { mcpSection } from "./prompt.ts";

async function canRun(): Promise<boolean> {
  return (await Deno.permissions.query({ name: "run" })).state === "granted";
}

const FIXTURE = new URL("./testdata/echo_server.ts", import.meta.url).pathname;

/** Temp BOUGH_MCP_DIR with the fixture registered as "echo"; no sandbox. */
async function withManager(fn: (m: McpManager, workspace: string) => Promise<void>) {
  const dir = Deno.makeTempDirSync({ prefix: "bough-mcp-" });
  const workspace = Deno.makeTempDirSync({ prefix: "bough-ws-" });
  Deno.env.set("BOUGH_MCP_DIR", dir);
  const manager = new McpManager();
  try {
    saveRegistry({
      servers: {
        echo: { command: Deno.execPath(), args: ["run", "--quiet", "--no-config", FIXTURE] },
        remote: { url: "http://127.0.0.1:9/mcp" }, // unreachable — degrades to an error entry
      },
    });
    await fn(manager, workspace);
  } finally {
    await manager.dropAll();
    Deno.env.delete("BOUGH_MCP_DIR");
  }
}

Deno.test("manager: ensure connects, calls map results, errors throw", async () => {
  if (!(await canRun())) return;
  await withManager(async (m, workspace) => {
    const catalog = await m.ensure("s1", ["echo", "remote", "ghost"], { workspace });
    assertEquals(catalog.map((c) => c.name), ["echo", "remote", "ghost"]);
    assertEquals(catalog[0].tools.map((t) => t.name), ["echo", "scream", "boom"]);
    assert((catalog[1].error ?? "").length > 0); // remote connect failed, named not thrown
    assertStringIncludes(catalog[2].error ?? "", "not in the registry");

    // structuredContent preferred; plain text falls back; isError throws
    assertEquals(await m.call("s1", "echo", "echo", { text: "hi" }), { echoed: "hi" });
    assertEquals(await m.call("s1", "echo", "scream", { text: "hi" }), "HI");
    await assertRejects(() => m.call("s1", "echo", "boom", {}), Error, "kaboom");
    await assertRejects(() => m.call("s1", "echo", "nope", {}), Error, 'no tool "nope"');
    await assertRejects(() => m.call("s2", "echo", "echo", {}), Error, "not connected");

    // second ensure reuses the connection (no second child)
    const again = await m.ensure("s1", ["echo"], { workspace });
    assertEquals(again[0].tools.length, 3);
    assertEquals(m.statuses("s1").length, 1);
  });
});

Deno.test("manager: restart respawns; drop disconnects", async () => {
  if (!(await canRun())) return;
  await withManager(async (m, workspace) => {
    await m.ensure("s1", ["echo"], { workspace });
    const st = await m.restart("s1", "echo");
    assertEquals([st.alive, st.toolCount], [true, 3]);
    assertEquals(await m.call("s1", "echo", "echo", { text: "still up" }), { echoed: "still up" });
    await m.drop("s1", "echo");
    assertEquals(m.statuses("s1"), []);
    await assertRejects(() => m.call("s1", "echo", "echo", {}), Error, "not connected");
  });
});

Deno.test("prompt section: compact catalog, params, failures named", () => {
  const section = mcpSection([
    {
      name: "echo",
      tools: [
        {
          name: "echo",
          description: "Echo the text back.\nSecond line dropped.",
          inputSchema: {
            properties: { text: { type: "string" }, loud: { type: "boolean" } },
            required: ["text"],
          },
        },
        { name: "ping" },
      ],
    },
    { name: "linear", tools: [], error: "not in the registry" },
  ]);
  assertStringIncludes(section, "# MCP tools");
  assertStringIncludes(section, "await mcp(server, tool, args)");
  assertStringIncludes(section, 'server "echo" (2 tools):');
  assertStringIncludes(section, "- echo({text, loud?}) — Echo the text back.");
  assertStringIncludes(section, "- ping()");
  assertStringIncludes(section, 'server "linear": UNAVAILABLE — not in the registry');
  assertEquals(section.includes("Second line"), false);
  assertEquals(mcpSection([]), "");
});
