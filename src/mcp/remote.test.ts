import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { McpRemoteClient } from "./remote.ts";
import { McpManager } from "./manager.ts";
import { saveRegistry } from "./config.ts";

/**
 * Minimal streamable-http MCP server (JSON response mode): POSTed JSON-RPC
 * requests get one application/json response; notifications get a 202. Same tool
 * set as the stdio fixture, two tools/list pages to exercise the cursor loop.
 */
function startFixture(): { url: string; close: () => Promise<void> } {
  const handler = async (req: Request): Promise<Response> => {
    if (req.method !== "POST") return new Response("method not allowed", { status: 405 });
    const msg = await req.json() as {
      id?: number;
      method?: string;
      params?: { cursor?: string; name?: string; arguments?: Record<string, unknown> };
    };
    if (msg.id === undefined) return new Response(null, { status: 202 }); // notification
    const respond = (result: unknown) =>
      new Response(JSON.stringify({ jsonrpc: "2.0", id: msg.id, result }), {
        headers: { "content-type": "application/json" },
      });
    if (msg.method === "initialize") {
      return respond({
        protocolVersion: "2025-06-18",
        capabilities: { tools: {} },
        serverInfo: { name: "http-fixture", version: "0" },
      });
    }
    if (msg.method === "tools/list") {
      return msg.params?.cursor === "p2"
        ? respond({
          tools: [{
            name: "boom",
            description: "Always fails.",
            inputSchema: { type: "object", properties: {} },
          }],
        })
        : respond({
          tools: [{
            name: "echo",
            description: "Echo the text back.",
            inputSchema: { type: "object", properties: { text: { type: "string" } } },
            annotations: { readOnlyHint: true },
          }],
          nextCursor: "p2",
        });
    }
    if (msg.method === "tools/call") {
      const { name, arguments: args = {} } = msg.params ?? {};
      if (name === "echo") {
        return respond({
          content: [{ type: "text", text: String(args.text) }],
          structuredContent: { echoed: args.text },
        });
      }
      return respond({ content: [{ type: "text", text: "kaboom" }], isError: true });
    }
    return new Response(
      JSON.stringify({
        jsonrpc: "2.0",
        id: msg.id,
        error: { code: -32601, message: "method not found" },
      }),
      { headers: { "content-type": "application/json" } },
    );
  };
  const server = Deno.serve({ port: 0, onListen: () => {} }, handler);
  const { port } = server.addr as Deno.NetAddr;
  return { url: `http://127.0.0.1:${port}/mcp`, close: () => server.shutdown() };
}

function withMcpDir(fn: () => Promise<void>): Promise<void> {
  Deno.env.set("BOUGH_MCP_DIR", Deno.makeTempDirSync({ prefix: "bough-mcp-remote-" }));
  return fn().finally(() => Deno.env.delete("BOUGH_MCP_DIR"));
}

Deno.test("remote client: SDK transport connects, paginates tools, calls round-trip", async () => {
  await withMcpDir(async () => {
    const fixture = startFixture();
    const client = await McpRemoteClient.connect({ server: "fix", url: fixture.url });
    try {
      const tools = await client.listTools();
      assertEquals(tools.map((t) => t.name), ["echo", "boom"]);
      assertEquals(tools[0].annotations?.readOnlyHint, true);
      const res = await client.callTool("echo", { text: "hi" });
      assertEquals(res.structuredContent, { echoed: "hi" });
      const boom = await client.callTool("boom", {});
      assertEquals(boom.isError, true);
    } finally {
      await client.close();
      await fixture.close();
    }
    assertEquals(client.alive, false);
  });
});

Deno.test("manager: a url server ensures and calls like a stdio one", async () => {
  await withMcpDir(async () => {
    const fixture = startFixture();
    const manager = new McpManager();
    try {
      saveRegistry({ servers: { fix: { url: fixture.url } } });
      const catalog = await manager.ensure("s1", ["fix"], { workspace: "/tmp" });
      assertEquals(catalog[0].error, undefined);
      assertEquals(catalog[0].tools.map((t) => t.name), ["echo", "boom"]);
      assertEquals(await manager.call("s1", "fix", "echo", { text: "yo" }), { echoed: "yo" });
      await assertRejects(() => manager.call("s1", "fix", "boom", {}), Error, "kaboom");
    } finally {
      await manager.dropAll();
      await fixture.close();
    }
  });
});

Deno.test("manager: an unreachable url server degrades to a catalog error", async () => {
  await withMcpDir(async () => {
    const manager = new McpManager();
    try {
      saveRegistry({ servers: { dead: { url: "http://127.0.0.1:9/mcp" } } });
      const catalog = await manager.ensure("s1", ["dead"], { workspace: "/tmp" });
      assertStringIncludes(catalog[0].error ?? "", "");
      assertEquals(catalog[0].tools, []);
    } finally {
      await manager.dropAll();
    }
  });
});
