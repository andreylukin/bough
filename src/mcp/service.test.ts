/**
 * The MCP service: connections owned by the process, not by a conversation.
 *
 * Hermetic — a temp registry file and an injected connector, so nothing spawns, no
 * socket is opened, and the assertions are about WHICH servers the process holds
 * open rather than about any real endpoint.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { McpConnection } from "./client.ts";
import { saveRegistry, setActivation } from "./config.ts";
import { McpManager, SHARED_SCOPE } from "./manager.ts";
import { reconcileMcp, reconcileSummary } from "./service.ts";

function tmpRegistry(): string {
  return join(mkdtempSync(join(tmpdir(), "bough-mcp-service-")), "mcp.json");
}

function fakeConnection(name: string): McpConnection {
  let alive = true;
  return {
    name,
    get alive() {
      return alive;
    },
    stderrTail: "",
    listTools: () => Promise.resolve([{ name: "echo", description: "", inputSchema: {} }]),
    callTool: () => Promise.resolve({ content: [], isError: false }),
    close: () => {
      alive = false;
      return Promise.resolve();
    },
  } as unknown as McpConnection;
}

test("granted REMOTE servers are connected with no conversation in existence", async () => {
  // The whole point: "is Slack connected?" must have a process-level answer. Every
  // connection used to be made by a turn, in a conversation's name, so the honest
  // answer in a fresh conversation was always "no".
  const file = tmpRegistry();
  saveRegistry({
    servers: {
      remote: { url: "https://a.example/mcp" },
      ungranted: { url: "https://b.example/mcp" },
      local: { command: "echo", args: [] },
    },
  }, { file });
  setActivation(undefined, "remote", true, { file });
  setActivation(undefined, "local", true, { file }); // granted, but a subprocess

  let connects = 0;
  const mgr = new McpManager({
    config: { file },
    connect: ({ name }) => {
      connects++;
      return Promise.resolve(fakeConnection(name));
    },
  });
  try {
    const r = await reconcileMcp({ manager: mgr, config: { file } });
    assert.deepEqual(r.connected, ["remote"]);
    assert.deepEqual(r.failed, []);
    // A stdio server is NOT started: its cwd is a conversation's checkout, so
    // connecting one at boot would spawn a process for a conversation that may never
    // happen, in a directory that is not its own.
    assert.equal(connects, 1);
    assert.deepEqual(mgr.statuses(SHARED_SCOPE).map((c) => c.server), ["remote"]);

    // Idempotent: a second pass reuses the live connection rather than reopening it.
    await reconcileMcp({ manager: mgr, config: { file } });
    assert.equal(connects, 1);
  } finally {
    await mgr.dropAll();
  }
});

test("a revoked server is disconnected, not left serving from an open connection", async () => {
  // A permission that outlives its revocation is the one thing this layer must never
  // produce: the grant is gone from the file, so the connection has to go too.
  const file = tmpRegistry();
  saveRegistry({ servers: { remote: { url: "https://a.example/mcp" } } }, { file });
  setActivation(undefined, "remote", true, { file });
  const mgr = new McpManager({
    config: { file },
    connect: ({ name }) => Promise.resolve(fakeConnection(name)),
  });
  try {
    await reconcileMcp({ manager: mgr, config: { file } });
    assert.equal(mgr.statuses(SHARED_SCOPE).length, 1);

    setActivation(undefined, "remote", false, { file });
    const r = await reconcileMcp({ manager: mgr, config: { file } });
    assert.deepEqual(r.closed, ["remote"]);
    assert.deepEqual(r.connected, []);
    assert.equal(mgr.statuses(SHARED_SCOPE).length, 0);
  } finally {
    await mgr.dropAll();
  }
});

test("a server that will not connect is a reported failure, never a throw", async () => {
  // Boot must not depend on a third party being up: the reason belongs on a row in
  // the panel, which is where someone can act on it.
  const file = tmpRegistry();
  saveRegistry({ servers: { remote: { url: "https://a.example/mcp" } } }, { file });
  setActivation(undefined, "remote", true, { file });
  const mgr = new McpManager({
    config: { file },
    connect: () => Promise.reject(new Error("connection refused")),
  });
  try {
    const r = await reconcileMcp({ manager: mgr, config: { file } });
    assert.deepEqual(r.connected, []);
    assert.equal(r.failed.length, 1);
    assert.match(r.failed[0].error, /connection refused/);
    assert.match(reconcileSummary(r) ?? "", /remote failed \(.*connection refused/);
  } finally {
    await mgr.dropAll();
  }
});

test("nothing granted is a quiet no-op, and says nothing in the boot log", async () => {
  const file = tmpRegistry();
  saveRegistry({ servers: { remote: { url: "https://a.example/mcp" } } }, { file });
  const mgr = new McpManager({ config: { file }, connect: () => Promise.reject(new Error("x")) });
  const r = await reconcileMcp({ manager: mgr, config: { file } });
  assert.deepEqual(r, { connected: [], failed: [], closed: [] });
  assert.equal(reconcileSummary(r), null);
});
