/**
 * Tests for the stdio MCP client, driven against the fixture server in
 * `testdata/echo_server.ts` — a real child process speaking real JSON-RPC.
 *
 * Half of these tests are about the happy path (handshake, paginated
 * `tools/list`, `tools/call`) and half are about the property the module exists
 * for: **a server that does not work fails by name in bounded time.** Each failure
 * mode gets its own test with a deadline measured in hundreds of milliseconds, so
 * a regression that reintroduces a hang shows up as a test that fails, not as a
 * test suite that never finishes.
 *
 * Hermetic: no network, no registry file, and the child's environment is composed
 * by `config.childEnv` exactly as production composes it.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { McpError } from "../errors.ts";
import { childEnv, ServerConfig } from "./config.ts";
import {
  killAllMcpServers,
  liveMcpServerCount,
  McpStdioClient,
  type McpTimeouts,
} from "./client.ts";

const FIXTURE = new URL("./testdata/echo_server.ts", import.meta.url).pathname;

function connectFixture(
  args: string[] = [],
  timeouts: McpTimeouts = {},
): Promise<McpStdioClient> {
  const server = ServerConfig.parse({
    command: process.execPath,
    args: ["run", FIXTURE, ...args],
  });
  return McpStdioClient.connect({
    name: "echo",
    argv: [server.command!, ...server.args],
    env: childEnv(server),
    // Short by production standards, long enough that a loaded machine still
    // completes the handshake.
    timeouts: { connectMs: 20_000, requestMs: 20_000, callMs: 20_000, ...timeouts },
  });
}

test("handshake, paginated tools/list, tools/call, close", async () => {
  const client = await connectFixture();
  try {
    assert.equal(client.alive, true);
    assert.equal(client.serverInfo?.name, "echo-fixture");
    assert.equal(client.serverInfo?.protocolVersion, "2025-06-18");

    const tools = await client.listTools();
    // Both pages, in order. The nameless entry is dropped; the one with a sloppy
    // inputSchema is KEPT, because it is callable.
    assert.deepEqual(tools.map((t) => t.name), ["echo", "scream", "boom", "die", "slow", "loose"]);
    assert.equal(tools[0].annotations?.readOnlyHint, true);
    assert.deepEqual(tools[0].inputSchema?.required, ["text"]);
    assert.deepEqual(Object.keys(tools[5].inputSchema?.properties ?? {}), ["q"]);

    const echoed = await client.callTool("echo", { text: "hi" });
    assert.deepEqual(echoed.structuredContent, { echoed: "hi" });
    assert.equal(echoed.content?.[0]?.text, "hi");
    assert.equal(echoed.isError ?? false, false);

    // A tool that fails is DATA, not an exception.
    const boom = await client.callTool("boom", {});
    assert.equal(boom.isError, true);
    assert.equal(boom.content?.[0]?.text, "kaboom");

    // A tool advertised with a sloppy schema is still callable.
    const loose = await client.callTool("loose", { q: "x" });
    assert.equal(loose.content?.[0]?.text, "q=x");
  } finally {
    await client.close();
  }
  assert.equal(client.alive, false);
});

test("a server that logs to stdout still connects", async () => {
  const client = await connectFixture(["--noise"]);
  try {
    assert.equal((await client.listTools()).length, 6);
  } finally {
    await client.close();
  }
});

test("a server that dies mid-call fails that call, by name, with its stderr", async () => {
  const client = await connectFixture();
  try {
    const error = await client.callTool("die", {}).then(
      () => undefined,
      (e: unknown) => e,
    );
    assert.ok(error instanceof McpError, `expected McpError, got ${error}`);
    assert.equal(error.status, 502);
    assert.match(error.message, /MCP server "echo" exited/);
    assert.match(error.message, /code 3/);
    // The diagnostic the user needs is attached, not buried in a log file.
    assert.match(error.message, /asked to die/);
    assert.equal(client.alive, false);

    // And the connection stays failed rather than hanging the next call.
    await assert.rejects(
      () => client.listTools(),
      (e: unknown) => e instanceof McpError && /is not running/.test(e.message),
    );
  } finally {
    await client.close();
  }
});

test("a call the server never answers fails on its deadline, server still alive", async () => {
  const client = await connectFixture([], { callMs: 300 });
  try {
    const error = await client.callTool("slow", {}).then(() => undefined, (e: unknown) => e);
    assert.ok(error instanceof McpError, `expected McpError, got ${error}`);
    assert.equal(error.status, 504);
    assert.match(error.message, /MCP tools\/call on server "echo" timed out after 300ms/);
    // The server is fine — one call timing out must not condemn the connection.
    assert.equal(client.alive, true);
    assert.equal((await client.listTools()).length, 6);
  } finally {
    await client.close();
  }
});

test("a server that starts and never handshakes fails on the connect deadline", async () => {
  const started = Date.now();
  const error = await connectFixture(["--deaf"], { connectMs: 300 }).then(
    () => undefined,
    (e: unknown) => e,
  );
  assert.ok(error instanceof McpError, `expected McpError, got ${error}`);
  assert.equal(error.status, 504);
  assert.match(error.message, /MCP initialize on server "echo" timed out after 300ms/);
  // Bounded, and bounded by the deadline we set — not by a production default.
  assert.ok(Date.now() - started < 10_000, "connect must not outlast its deadline");
});

test("a binary that does not exist fails at spawn, naming the command", async () => {
  const error = await McpStdioClient.connect({
    name: "ghost",
    argv: ["/nonexistent/bough-mcp-server", "--serve"],
    env: {},
  }).then(() => undefined, (e: unknown) => e);
  assert.ok(error instanceof McpError, `expected McpError, got ${error}`);
  assert.equal(error.status, 502);
  assert.match(error.message, /MCP server "ghost" failed to start/);
  assert.match(error.message, /bough-mcp-server/);
});

test("a process that exits immediately fails the handshake instead of hanging", async () => {
  const error = await McpStdioClient.connect({
    name: "quitter",
    argv: [process.execPath, "-e", "process.exit(1)"],
    env: childEnv(ServerConfig.parse({ command: process.execPath })),
    timeouts: { connectMs: 20_000 },
  }).then(() => undefined, (e: unknown) => e);
  assert.ok(error instanceof McpError, `expected McpError, got ${error}`);
  assert.match(error.message, /MCP server "quitter" exited/);
});

test("requests after close reject rather than resolving on a dead pipe", async () => {
  const client = await connectFixture();
  await client.close();
  await assert.rejects(
    () => client.listTools(),
    (e: unknown) => e instanceof McpError && /is not running/.test(e.message),
  );
  await assert.rejects(() => client.callTool("echo", { text: "hi" }), McpError);
});

test("shutdown kills every live server — the wiring server/main.ts calls", async () => {
  const before = liveMcpServerCount();
  const a = await connectFixture();
  const b = await connectFixture();
  assert.equal(liveMcpServerCount(), before + 2);

  assert.equal(killAllMcpServers() >= 2, true);
  assert.equal(liveMcpServerCount(), before);

  // The children are actually gone, and closing a killed client is still safe.
  await a.close();
  await b.close();
  assert.equal(a.alive, false);
  assert.equal(b.alive, false);
});
