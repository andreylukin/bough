import { assertEquals, assertRejects, assertStringIncludes } from "jsr:@std/assert@1";
import { saveRegistry } from "./config.ts";
import { createLspBridge, LSP_SERVER, lspAvailable, type LspManager } from "./lsp.ts";
import type { ServerCatalog, SpawnCtx } from "./manager.ts";

function withMcpDir(fn: () => void) {
  const dir = Deno.makeTempDirSync({ prefix: "bough-mcp-" });
  Deno.env.set("BOUGH_MCP_DIR", dir);
  try {
    fn();
  } finally {
    Deno.env.delete("BOUGH_MCP_DIR");
  }
}

const spawn: SpawnCtx = { workspace: "/ws" };

/** A fake manager that records ensures/calls and answers with canned results. */
function fakeManager(opts: { ensureError?: string } = {}) {
  const ensures: string[][] = [];
  const calls: Array<{ server: string; tool: string; args: unknown }> = [];
  const manager: LspManager = {
    ensure: (_s, servers) => {
      ensures.push(servers);
      return Promise.resolve(
        servers.map((name): ServerCatalog =>
          opts.ensureError ? { name, tools: [], error: opts.ensureError } : { name, tools: [] }
        ),
      );
    },
    call: (_s, server, tool, args) => {
      calls.push({ server, tool, args });
      return Promise.resolve(`${tool} ok`);
    },
  };
  return { manager, ensures, calls };
}

Deno.test("lspAvailable: true out of the box (builtin) and with a user override", () => {
  withMcpDir(() => {
    assertEquals(lspAvailable(), true); // BUILTIN_SERVERS ships the backend
    saveRegistry({ servers: { [LSP_SERVER]: { command: "my-serena", args: [] } } });
    assertEquals(lspAvailable(), true);
  });
});

Deno.test("bridge: first call connects + activates the workspace once, verbs map to tools", async () => {
  const { manager, ensures, calls } = fakeManager();
  const bridge = createLspBridge("s1", spawn, manager);

  const out = await bridge.call("refs", { name_path: "Foo/bar", relative_path: "src/foo.ts" });
  assertEquals(out, "find_referencing_symbols ok");
  assertEquals(ensures, [[LSP_SERVER]]);
  assertEquals(calls[0], {
    server: LSP_SERVER,
    tool: "activate_project",
    args: { project: "/ws" },
  });
  assertEquals(calls[1].tool, "find_referencing_symbols");

  // Second call reuses the memoized connect — no new ensure, no re-activation.
  await bridge.call("def", { name_path: "Foo" });
  assertEquals(ensures.length, 1);
  assertEquals(calls.map((c) => c.tool), [
    "activate_project",
    "find_referencing_symbols",
    "find_declaration",
  ]);
});

Deno.test("bridge: unknown verb rejects and names the verbs, without connecting", async () => {
  const { manager, ensures } = fakeManager();
  const bridge = createLspBridge("s1", spawn, manager);
  const err = await assertRejects(() => bridge.call("hover", {}), Error);
  assertStringIncludes(err.message, 'unknown lsp verb "hover"');
  assertStringIncludes(err.message, "refs");
  assertEquals(ensures.length, 0);
});

Deno.test("bridge: a failed connect surfaces the catalog error and is retried next call", async () => {
  const { manager, ensures, calls } = fakeManager({ ensureError: "spawn failed" });
  const bridge = createLspBridge("s1", spawn, manager);
  const err = await assertRejects(() => bridge.call("find", {}), Error);
  assertStringIncludes(err.message, `lsp backend "${LSP_SERVER}" unavailable: spawn failed`);
  assertEquals(calls.length, 0); // never reached activate/tool call

  // The memo was cleared: the next call ensures again.
  await assertRejects(() => bridge.call("find", {}), Error);
  assertEquals(ensures.length, 2);
});
