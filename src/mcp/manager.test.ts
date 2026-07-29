/**
 * Tests for the connection manager, the grant that decides who may ask, and the two
 * program verbs over both.
 *
 * Three of these carry the acceptance criteria and each one is a failure mode that
 * would otherwise be found in production, by a user, as an agent that quietly cannot
 * do its job:
 *
 *   1. **A subagent inherits its spawner's grant.** Driven end to end through the
 *      real `launchSubagent` against a real database, a real bus and the real turn
 *      runner, with a scripted fake `LlmClient` — the child's own ctx is captured and
 *      its `mcp()` really calls a really-spawned server. A child that resolved its
 *      own grant would resolve to nothing (it has no activations), so the assertion
 *      that would pass a broken implementation is "the child's grant is empty"; this
 *      test asserts the opposite, from the child's side of the bridge.
 *   2. **A revoked grant is visible to the very next `mcpStatus()`.** Not the next
 *      turn — the next CALL, inside one program, with nothing else touched. A cache
 *      of any lifetime fails this.
 *   3. **A down server degrades to a named status.** It does not hang, it does not
 *      escape as an unhandled rejection, and it does not vanish: `mcpStatus()` shows
 *      a `failed` row naming the server and the reason.
 *
 * Hermetic: the registry is a temp file passed as `{file}` on every call, the clock
 * is injected, and the only child processes are the fixture server in
 * `testdata/echo_server.ts`, which needs no permissions and no network. Assertions
 * come from `node:assert/strict` — jsr.io is not reachable here, and a test that
 * cannot run offline does not belong in `bun test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { McpError } from "../errors.ts";
import { launchSubagent } from "../agents/subagent.ts";
import { createMcpHostFns } from "../hostfn/mcp.ts";
import type { ProgramResult } from "../harness/protocol.ts";
import { TurnRegistry } from "../turn/queue.ts";
import { baseHostFns, type ProgramRunner, STOP, type TurnDeps } from "../turn/runner.ts";
import { runProgram } from "../harness/vm.ts";
import type { HostFns, LlmBlock, LlmClient, TurnCtx } from "../types.ts";
import { saveRegistry, setActivation, ttlToExpires } from "./config.ts";
import type { McpConnection, McpToolInfo } from "./client.ts";
import {
  bindTurnGrant,
  type Connector,
  McpManager,
  requireGranted,
  resolveGrant,
  setMcpManager,
} from "./manager.ts";
import {
  connectMcpServerH,
  deleteMcpServerH,
  getMcpServersH,
  mcpStatusFor,
  putMcpServerH,
  putMcpServersH,
  restartMcpServerH,
  setMcpActivationH,
} from "./status.ts";
import { createHandler, route } from "../server/app.ts";
import type { AppCtx } from "../types.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const FIXTURE = new URL("./testdata/echo_server.ts", import.meta.url).pathname;

function tmpRegistry(): string {
  return join(mkdtempSync(join(tmpdir(), "bough-mcp-manager-")), "mcp.json");
}

/** A registry holding the fixture server under `name`, plus anything extra. */
function seedRegistry(
  file: string,
  name = "echo",
  extra: Record<string, unknown> = {},
): void {
  saveRegistry({
    servers: {
      [name]: {
        command: process.execPath,
        args: ["run", FIXTURE],
      },
      ...extra,
    },
  }, { file });
}

/** Deadlines short enough that a regression reads as a failure, not a hung suite. */
const TIMEOUTS = { connectMs: 20_000, requestMs: 20_000, callMs: 20_000 };

function manager(file: string, connect?: Connector): McpManager {
  return new McpManager({
    config: { file },
    timeouts: TIMEOUTS,
    ...(connect ? { connect } : {}),
  });
}

/**
 * A connection that never spawns anything — for the paths where the point is the
 * manager's bookkeeping, not a real server.
 */
function fakeConnection(name: string, tools: string[]): McpConnection {
  let alive = true;
  return {
    name,
    listTools: () =>
      Promise.resolve(tools.map((t): McpToolInfo => ({ name: t, description: `the ${t} tool` }))),
    callTool: (tool: string, args: unknown) =>
      Promise.resolve({ content: [{ type: "text", text: `${tool}:${JSON.stringify(args)}` }] }),
    close: () => {
      alive = false;
      return Promise.resolve();
    },
    get alive() {
      return alive;
    },
    stderrTail: "",
  };
}

/** A minimal turn ctx. `db`/`bus` are only read by paths these tests do not take. */
function turnCtx(sessionId: string, extra: Partial<TurnCtx> = {}): TurnCtx {
  return {
    db: undefined as unknown as TurnCtx["db"],
    bus: undefined as unknown as TurnCtx["bus"],
    sessionId,
    turnId: `turn-${sessionId}`,
    messageId: `msg-${sessionId}`,
    workspace: process.cwd(),
    model: "claude-test-model",
    signal: new AbortController().signal,
    depth: 0,
    ...extra,
  };
}

function hostFns(ctx: TurnCtx, mgr: McpManager, file: string): Pick<HostFns, "mcp" | "mcpStatus"> {
  return createMcpHostFns(ctx, { manager: mgr, config: { file }, auth: () => false });
}

async function statusOf(fns: Pick<HostFns, "mcp" | "mcpStatus">) {
  return JSON.parse(await fns.mcpStatus!()) as ReturnType<typeof mcpStatusFor>;
}

// ---------------------------------------------------------------------------
// Grants
// ---------------------------------------------------------------------------

test("a registered server is not a callable one until a human grants it", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const mgr = manager(file, ({ name }) => Promise.resolve(fakeConnection(name, ["echo"])));
  const ctx = bindTurnGrant(turnCtx("s1"), { file });
  const fns = hostFns(ctx, mgr, file);
  try {
    const refused = await fns.mcp!("echo", "echo", "{}").then(() => undefined, (e: unknown) => e);
    assert.ok(refused instanceof McpError, `expected McpError, got ${refused}`);
    assert.equal(refused.status, 403);
    assert.match(refused.message, /registered but not granted/);
    // The message says who can fix it, so the next round is not spent trying.
    assert.match(refused.message, /a program cannot grant itself one/);

    // An unregistered name is a different failure, and says so.
    const unknown = await fns.mcp!("nope", "echo", "{}").then(() => undefined, (e: unknown) => e);
    assert.ok(unknown instanceof McpError);
    assert.equal(unknown.status, 404);
    assert.match(unknown.message, /Registered servers: echo/);

    setActivation("s1", "echo", true, { file });
    assert.equal(
      await fns.mcp!("echo", "echo", '{"text":"hi"}'),
      JSON.stringify('echo:{"text":"hi"}'),
    );
  } finally {
    await mgr.dropAll();
  }
});

test("a lapsed grant fails closed, and the clock that decides is injected", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const mgr = manager(file, ({ name }) => Promise.resolve(fakeConnection(name, ["echo"])));
  const ctx = turnCtx("s1");
  let now = 1_000_000;
  const fns = createMcpHostFns(ctx, {
    manager: mgr,
    config: { file },
    auth: () => false,
    now: () => now,
  });
  try {
    setActivation("s1", "echo", true, { file, expires: ttlToExpires("2h", now) });
    assert.deepEqual((await statusOf(fns)).active, ["echo"]);
    assert.ok(await fns.mcp!("echo", "echo", "{}"));

    now += 3 * 3_600_000; // two hours later, plus change
    assert.deepEqual((await statusOf(fns)).active, [], "the grant lapsed with no sweep");
    await assert.rejects(
      () => fns.mcp!("echo", "echo", "{}"),
      (e: unknown) => e instanceof McpError && e.status === 403,
    );
  } finally {
    await mgr.dropAll();
  }
});

// ---------------------------------------------------------------------------
// AC: a revoked grant is visible to the very next mcpStatus()
// ---------------------------------------------------------------------------

test("AC: a revoked grant is visible to the very next mcpStatus() call", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const mgr = manager(file, ({ name }) => Promise.resolve(fakeConnection(name, ["echo"])));
  const ctx = bindTurnGrant(turnCtx("s1"), { file });
  const fns = hostFns(ctx, mgr, file);
  try {
    setActivation("s1", "echo", true, { file });
    const before = await statusOf(fns);
    assert.deepEqual(before.active, ["echo"]);
    assert.deepEqual(Object.keys(before.registry.servers), ["echo"]);

    // Revoked between two calls of ONE program — no new turn, nothing else touched.
    setActivation("s1", "echo", false, { file });

    const after = await statusOf(fns);
    assert.deepEqual(after.active, [], "the very next status call reports the revocation");
    await assert.rejects(
      () => fns.mcp!("echo", "echo", "{}"),
      (e: unknown) => e instanceof McpError && e.status === 403,
      "and the call refuses too — status and enforcement read the same grant",
    );

    // The other direction, same call: a grant made mid-program is live immediately.
    setActivation("s1", "echo", true, { file });
    assert.deepEqual((await statusOf(fns)).active, ["echo"]);
  } finally {
    await mgr.dropAll();
  }
});

test("a registry edited on disk is visible to the very next status call", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const mgr = manager(file);
  const ctx = bindTurnGrant(turnCtx("s1"), { file });
  const fns = hostFns(ctx, mgr, file);
  try {
    assert.deepEqual(Object.keys((await statusOf(fns)).registry.servers), ["echo"]);
    seedRegistry(file, "echo", { linear: { url: "https://mcp.linear.app/mcp" } });
    const after = await statusOf(fns);
    assert.deepEqual(Object.keys(after.registry.servers).sort(), ["echo", "linear"]);
    // Remote servers, and only remote servers, carry an auth flag — and never a token.
    assert.deepEqual(after.auth, { linear: { authorized: false } });
    assert.ok(!JSON.stringify(after).includes("token"));
  } finally {
    await mgr.dropAll();
  }
});

// ---------------------------------------------------------------------------
// AC: a subagent inherits its spawner's grant
// ---------------------------------------------------------------------------

test("AC: a subagent inherits its spawner's grant and can call the server", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const db: SqliteDb = openDb(":memory:");
  const bus = new Bus();
  const mgr = manager(file);
  try {
    // The human granted the SPAWNER, and only the spawner.
    const spawner = db.createSession({
      id: crypto.randomUUID(),
      title: "the spawner",
      kind: "root",
      createdAt: 1_000,
      parentId: null,
      workspace: process.cwd(),
      originDir: process.cwd(),
    });
    const supervisor = db.createMessage({
      id: crypto.randomUUID(),
      sessionId: spawner.id,
      role: "supervisor",
      parts: [],
      pending: true,
      createdAt: 1_001,
    });
    setActivation(spawner.id, "echo", true, { file });

    const spawnerCtx = bindTurnGrant(
      turnCtx(spawner.id, {
        db,
        bus,
        llm: reportingLlm("done"),
        messageId: supervisor.id,
        workspace: process.cwd(),
      }),
      { file },
    );

    // The spawner's own view: one granted server.
    assert.deepEqual(await grantedNames(spawnerCtx, mgr, file), ["echo"]);

    let childCtx: TurnCtx | undefined;
    const launch = launchSubagent(spawnerCtx, "Ask the echo server to say hello.", {}, {
      turn: {
        registry: new TurnRegistry(),
        programFor: (c) => {
          childCtx = c;
          return fakeProgram();
        },
      } satisfies TurnDeps,
    });
    await launch.result;

    assert.ok(childCtx, "the child's turn ran");
    // The child is a session of its own with NO activations — the grant it holds can
    // only have come from its spawner.
    assert.deepEqual(resolveGrant({ sessionId: childCtx!.sessionId }, { file }), []);
    assert.deepEqual(childCtx!.mcpGrant, ["echo"], "the spawner's grant crossed the boundary");

    // And it is a real capability, not a label: the child's own bridge calls a real
    // server spawned in the child's turn.
    const childFns = hostFns(childCtx!, mgr, file);
    const said = JSON.parse(await childFns.mcp!("echo", "echo", '{"text":"hello"}'));
    assert.deepEqual(said, { echoed: "hello" });

    const childStatus = await statusOf(childFns);
    assert.deepEqual(childStatus.active, ["echo"], "and mcpStatus() tells it the same");
    assert.deepEqual(childStatus.connections.map((c) => [c.server, c.state]), [[
      "echo",
      "connected",
    ]]);

    // The grant is a SNAPSHOT taken at spawn: revoking the spawner's grant now does
    // not disarm a child that is already running on the human's authorization…
    setActivation(spawner.id, "echo", false, { file });
    assert.deepEqual(childCtx!.mcpGrant, ["echo"]);
    assert.ok(await childFns.mcp!("echo", "echo", '{"text":"still here"}'));
    // …while the spawner itself is refused on its very next call.
    assert.deepEqual(await grantedNames(spawnerCtx, mgr, file), []);
  } finally {
    await mgr.dropAll();
    db.close();
  }
});

test("an ungranted spawner hands its subagent nothing, not the global scope", () => {
  const file = tmpRegistry();
  seedRegistry(file);
  setActivation(undefined, "echo", true, { file }); // granted GLOBALLY
  try {
    // A top-level turn sees the global grant…
    assert.deepEqual(resolveGrant({ sessionId: "s1" }, { file }), ["echo"]);
    // …but a child spawned by a turn that held nothing stays at nothing. An empty
    // inherited grant is a grant, not an absent one — truthiness here would fall
    // through to the file and quietly widen it.
    assert.deepEqual(resolveGrant({ sessionId: "child", mcpGrant: [] }, { file }), []);
    assert.throws(
      () => requireGranted({ sessionId: "child", mcpGrant: [] }, "echo", { file }),
      (e: unknown) =>
        e instanceof McpError && e.status === 403 && /cannot widen it/.test(e.message),
    );
  } finally {
    // nothing to clean up: no connection was made
  }
});

test("bindTurnGrant is a live read, never a frozen array, and never overwrites", () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const ctx = bindTurnGrant(turnCtx("s1"), { file });
  assert.deepEqual(ctx.mcpGrant, []);
  setActivation("s1", "echo", true, { file });
  assert.deepEqual(ctx.mcpGrant, ["echo"], "re-read on every access");

  // An inherited grant is left exactly as it was: re-deriving it from the child's
  // own (empty) activations would revoke it.
  const child = bindTurnGrant(turnCtx("child", { mcpGrant: ["echo"] }), { file });
  assert.deepEqual(child.mcpGrant, ["echo"]);
  setActivation("s1", "echo", false, { file });
  assert.deepEqual(child.mcpGrant, ["echo"]);
});

// ---------------------------------------------------------------------------
// AC: a down server degrades to a named status
// ---------------------------------------------------------------------------

test("AC: a server that cannot start degrades to a named status, not a hang", async () => {
  const file = tmpRegistry();
  saveRegistry({
    servers: { broken: { command: "/nonexistent/mcp-server-binary", args: [] } },
  }, { file });
  const mgr = manager(file);
  const ctx = bindTurnGrant(turnCtx("s1"), { file });
  const fns = hostFns(ctx, mgr, file);
  try {
    setActivation("s1", "broken", true, { file });

    // The call fails by name, bounded, with the move that resolves it.
    const failed = await fns.mcp!("broken", "anything", "{}").then(
      () => undefined,
      (e: unknown) => e,
    );
    assert.ok(failed instanceof McpError, `expected McpError, got ${failed}`);
    assert.match(failed.message, /MCP server "broken" failed to start/);
    assert.match(failed.message, /nonexistent\/mcp-server-binary/);

    // And the status the model is told to read carries it as a NAMED row rather
    // than as a missing server or an empty catalog.
    const status = await statusOf(fns);
    assert.deepEqual(status.active, ["broken"], "still granted — it is broken, not revoked");
    const row = status.connections.find((c) => c.server === "broken");
    assert.ok(row, "a server that failed to start still has a status row");
    assert.equal(row!.state, "failed");
    assert.equal(row!.alive, false);
    assert.deepEqual(row!.tools, []);
    assert.match(row!.error ?? "", /failed to start/);
  } finally {
    await mgr.dropAll();
  }
});

test("a server that dies is reported as exited, and the next call restarts it", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const mgr = manager(file);
  const ctx = bindTurnGrant(turnCtx("s1"), { file });
  const fns = hostFns(ctx, mgr, file);
  try {
    setActivation("s1", "echo", true, { file });
    assert.ok(await fns.mcp!("echo", "echo", '{"text":"one"}'));

    // `die` takes the child down mid-call. That is a call failure, by name.
    const died = await fns.mcp!("echo", "die", "{}").then(() => undefined, (e: unknown) => e);
    assert.ok(died instanceof McpError, `expected McpError, got ${died}`);
    assert.match(died.message, /MCP server "echo" exited/);

    const status = await statusOf(fns);
    const row = status.connections.find((c) => c.server === "echo");
    assert.ok(row);
    assert.equal(row!.state, "exited");
    assert.match(row!.stderrTail ?? "", /asked to die/);

    // The next call reconnects rather than reporting a dead server forever.
    assert.deepEqual(JSON.parse(await fns.mcp!("echo", "echo", '{"text":"two"}')), {
      echoed: "two",
    });
    const healed = await statusOf(fns);
    assert.equal(healed.connections.find((c) => c.server === "echo")!.state, "connected");
  } finally {
    await mgr.dropAll();
  }
});

test("a tool failure is the server's own words, and an unknown tool names the real ones", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const mgr = manager(file);
  const ctx = bindTurnGrant(turnCtx("s1"), { file });
  const fns = hostFns(ctx, mgr, file);
  try {
    setActivation("s1", "echo", true, { file });

    const boom = await fns.mcp!("echo", "boom", "{}").then(() => undefined, (e: unknown) => e);
    assert.ok(boom instanceof McpError, `expected McpError, got ${boom}`);
    assert.match(boom.message, /MCP echo:boom failed: kaboom/);

    const typo = await fns.mcp!("echo", "ecko", "{}").then(() => undefined, (e: unknown) => e);
    assert.ok(typo instanceof McpError);
    assert.equal(typo.status, 404);
    assert.match(typo.message, /has no tool "ecko"/);
    assert.match(typo.message, /It advertises: echo, scream, boom, die, slow, loose/);

    // Bad JSON from the bridge is caught before anything is spawned or called.
    await assert.rejects(
      () => fns.mcp!("echo", "echo", "{not json"),
      (e: unknown) => e instanceof McpError && /not valid JSON/.test(e.message),
    );
  } finally {
    await mgr.dropAll();
  }
});

// ---------------------------------------------------------------------------
// Connection lifecycle
// ---------------------------------------------------------------------------

test("connections are per session, reused across calls, and reaped when idle", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  let spawned = 0;
  let clock = 0;
  const mgr = new McpManager({
    config: { file },
    now: () => clock,
    idleMs: 1_000,
    connect: ({ name }) => {
      spawned++;
      return Promise.resolve(fakeConnection(name, ["echo"]));
    },
  });
  try {
    setActivation(undefined, "echo", true, { file }); // global, so both sessions have it
    const one = hostFns(bindTurnGrant(turnCtx("s1"), { file }), mgr, file);
    const two = hostFns(bindTurnGrant(turnCtx("s2"), { file }), mgr, file);

    await one.mcp!("echo", "echo", "{}");
    await one.mcp!("echo", "echo", "{}");
    assert.equal(spawned, 1, "one child serves every call in a session");

    await two.mcp!("echo", "echo", "{}");
    assert.equal(spawned, 2, "a second session gets its own child — its own checkout");
    assert.equal(mgr.statuses().length, 2);
    assert.equal(mgr.statuses("s1").length, 1, "status is scoped to the session that asks");

    clock += 5_000; // both are now idle past the window
    await one.mcp!("echo", "echo", "{}");
    assert.equal(spawned, 3, "the idle child was reaped and a fresh one spawned");
    assert.equal(mgr.statuses().length, 1, "and the other session's idle child is gone too");
  } finally {
    await mgr.dropAll();
  }
});

test("a REMOTE server is one connection for every conversation", async () => {
  // Reported as "after I start the conversation, all of the mcps disconnect". Keying
  // by session is a statement about a subprocess and its cwd; a remote server has
  // neither, so every new conversation opened a second connection to the same
  // endpoint and showed the server as not connected until it did.
  const file = tmpRegistry();
  saveRegistry({ servers: { remote: { url: "https://mcp.example.com/mcp" } } }, { file });
  let connects = 0;
  const mgr = new McpManager({
    config: { file },
    connect: ({ name }) => {
      connects++;
      return Promise.resolve(fakeConnection(name, ["echo"]));
    },
  });
  try {
    setActivation(undefined, "remote", true, { file });
    const one = hostFns(bindTurnGrant(turnCtx("s1"), { file }), mgr, file);
    const two = hostFns(bindTurnGrant(turnCtx("s2"), { file }), mgr, file);

    await one.mcp!("remote", "echo", "{}");
    await two.mcp!("remote", "echo", "{}");
    assert.equal(connects, 1, "the second conversation reuses the first's connection");

    // …and BOTH conversations see it as connected. The panel asks per session, and a
    // shared connection that only one of them could see would report "not connected"
    // about a server that is connected and about to answer.
    assert.equal(mgr.statuses("s1").length, 1);
    assert.equal(mgr.statuses("s2").length, 1);
    assert.equal(mgr.statuses().length, 1, "one connection, not one per session");

    // Revoking from either conversation closes the shared one — a drop that missed it
    // would leave it serving every other conversation.
    await mgr.drop("s2", "remote");
    assert.equal(mgr.statuses().length, 0);
  } finally {
    await mgr.dropAll();
  }
});

test("dropServer closes every session's connection to one server", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const closed: string[] = [];
  const mgr = manager(file, ({ name, spawn }) => {
    const conn = fakeConnection(name, ["echo"]);
    return Promise.resolve({
      ...conn,
      close: () => {
        closed.push(`${name}@${spawn.workspace}`);
        return conn.close();
      },
      get alive() {
        return conn.alive;
      },
    });
  });
  try {
    setActivation(undefined, "echo", true, { file });
    await hostFns(turnCtx("s1", { workspace: "/w1" }), mgr, file).mcp!("echo", "echo", "{}");
    await hostFns(turnCtx("s2", { workspace: "/w2" }), mgr, file).mcp!("echo", "echo", "{}");
    assert.equal(mgr.statuses().length, 2);

    await mgr.dropServer("echo");
    assert.deepEqual(closed.sort(), ["echo@/w1", "echo@/w2"]);
    assert.deepEqual(mgr.statuses(), [], "no rows left, live or failed");
  } finally {
    await mgr.dropAll();
  }
});

test("ensure reports one broken server without taking the others down", async () => {
  const file = tmpRegistry();
  saveRegistry({
    servers: {
      good: { command: "/bin/true", args: [] },
      bad: { command: "/bin/false", args: [] },
    },
  }, { file });
  const mgr = manager(file, ({ name }) => {
    if (name === "bad") return Promise.reject(new McpError(502, `MCP server "bad" is broken`));
    return Promise.resolve(fakeConnection(name, ["one", "two"]));
  });
  try {
    const catalogs = await mgr.ensure("s1", ["good", "bad", "missing"], { workspace: "/w" });
    assert.deepEqual(catalogs.map((c) => c.name), ["good", "bad", "missing"]);
    assert.deepEqual(catalogs[0].tools.map((t) => t.name), ["one", "two"]);
    assert.equal(catalogs[0].error, undefined);
    assert.match(catalogs[1].error ?? "", /is broken/);
    assert.match(catalogs[2].error ?? "", /not registered/);

    // Both failures are visible in the status surface, named.
    const rows = mgr.statuses("s1");
    assert.deepEqual(rows.map((r) => [r.server, r.state]), [
      ["bad", "failed"],
      ["good", "connected"],
      ["missing", "failed"],
    ]);
  } finally {
    await mgr.dropAll();
  }
});

test("status carries the live tool catalog, so a fresh call is enough to act on", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const mgr = manager(file);
  const ctx = bindTurnGrant(turnCtx("s1"), { file });
  const fns = hostFns(ctx, mgr, file);
  try {
    setActivation("s1", "echo", true, { file });
    assert.deepEqual((await statusOf(fns)).connections, [], "status never connects on its own");

    await fns.mcp!("echo", "echo", '{"text":"x"}');
    const row = (await statusOf(fns)).connections[0];
    assert.equal(row.server, "echo");
    assert.equal(row.toolCount, 6);
    assert.deepEqual(row.tools, ["echo", "scream", "boom", "die", "slow", "loose"]);
  } finally {
    await mgr.dropAll();
  }
});

// ---------------------------------------------------------------------------
// Turn-runner fakes, kept at the bottom because they are scaffolding
// ---------------------------------------------------------------------------

/** One round of text plus `stop` — the shortest complete turn there is. */
function reportingLlm(report: string): LlmClient {
  let calls = 0;
  return {
    run() {
      calls++;
      const content: LlmBlock[] = [
        { type: "text", text: report },
        { type: "tool_use", id: `stop-${calls}`, name: STOP, input: {} },
      ];
      return Promise.resolve({ content, stopReason: "tool_use" });
    },
  };
}

/** A program runner that never spawns a worker. */
function fakeProgram(): ProgramRunner {
  return () => Promise.resolve({ ok: true, logs: ["ok"] } satisfies ProgramResult);
}

/** What `mcpStatus()` says this turn may call, through the real bridge. */
async function grantedNames(ctx: TurnCtx, mgr: McpManager, file: string): Promise<string[]> {
  return (await statusOf(hostFns(ctx, mgr, file))).active;
}

// ---------------------------------------------------------------------------
// The HTTP surface
// ---------------------------------------------------------------------------
//
// Driven through the real dispatcher with a fabricated ctx and an in-memory
// database — no socket, no port (plan §7). `BOUGH_HOME` points the default registry
// path at a temp root, because these handlers read the DEFAULT paths: that is the
// half a `{file}`-injected test cannot cover, and the half a user actually uses.

/** Only this task's entries, so the test does not depend on the whole table's order. */
const TABLE = [
  route("GET", "/mcp/servers", getMcpServersH),
  route("PUT", "/mcp/servers", putMcpServersH),
  route("PUT", "/mcp/servers/:name", putMcpServerH),
  route("DELETE", "/mcp/servers/:name", deleteMcpServerH),
  route("POST", "/mcp/servers/:name/connect", connectMcpServerH),
  route("POST", "/mcp/servers/:name/restart", restartMcpServerH),
  route("POST", "/mcp/servers/:name/enable", setMcpActivationH(true)),
  route("POST", "/mcp/servers/:name/disable", setMcpActivationH(false)),
];

/** Point `BOUGH_HOME` at a fresh temp root, then put the environment back. */
async function withBoughHome(body: (home: string) => Promise<void>): Promise<void> {
  const home = mkdtempSync(join(tmpdir(), "bough-mcp-home-"));
  const previous = process.env.BOUGH_HOME;
  process.env.BOUGH_HOME = home;
  try {
    await body(home);
  } finally {
    if (previous === undefined) delete process.env.BOUGH_HOME;
    else process.env.BOUGH_HOME = previous;
    rmSync(home, { recursive: true });
  }
}

test("the API registers, grants, connects and revokes — and every reply is the state", async () => {
  await withBoughHome(async () => {
    const db = openDb(":memory:");
    const bus = new Bus({ onListenerError: () => {} });
    const call = createHandler({ db, bus } as AppCtx, { routes: TABLE });
    // The process manager, with a fake transport: the routes reach the SAME manager
    // a turn does, which is the property that makes `connect` a proof of anything.
    const previous = setMcpManager(
      new McpManager({ connect: ({ name }) => Promise.resolve(fakeConnection(name, ["echo"])) }),
    );
    const session = db.createSession({
      id: crypto.randomUUID(),
      title: "s",
      kind: "root",
      createdAt: 1,
      parentId: null,
      workspace: process.cwd(),
      originDir: process.cwd(),
    });
    const q = `?session=${session.id}`;
    try {
      const empty = await (await call(new Request("http://x/mcp/servers"))).json();
      assert.deepEqual(empty, { registry: { servers: {} }, auth: {}, active: [], connections: [] });

      // Register one. The reply is the whole state, not an ack.
      const put = await call(
        new Request(`http://x/mcp/servers/echo`, {
          method: "PUT",
          body: JSON.stringify({ command: "/bin/echo", args: ["hi"], cwd: "/tmp" }),
        }),
      );
      assert.equal(put.status, 200);
      const registered = await put.json();
      assert.equal(registered.registry.servers.echo.command, "/bin/echo");
      // `cwd` survived: the entry is validated by the schema the FILE is written
      // with, not by a narrower wire subset that would silently drop it.
      assert.equal(registered.registry.servers.echo.cwd, "/tmp");
      assert.deepEqual(registered.active, [], "registering granted nothing");

      // An invalid entry is a 400 whose message names the fix.
      const bad = await call(
        new Request(`http://x/mcp/servers/echo`, { method: "PUT", body: JSON.stringify({}) }),
      );
      assert.equal(bad.status, 400);
      assert.match((await bad.json()).error, /exactly one of `command` .* or `url`/);

      // Enabling is the grant, and it is scoped to the session that asked.
      const enabled = await (await call(
        new Request(`http://x/mcp/servers/echo/enable`, {
          method: "POST",
          body: JSON.stringify({ sessionId: session.id, ttl: "2h" }),
        }),
      )).json();
      assert.deepEqual(enabled.active, ["echo"]);
      assert.deepEqual(enabled.scope, { sessionId: session.id });
      const other = await (await call(new Request(`http://x/mcp/servers`))).json();
      assert.deepEqual(other.active, [], "another scope sees nothing");

      // Connect proves the command works and reports the catalog.
      const connected = await (await call(
        new Request(`http://x/mcp/servers/echo/connect${q}`, { method: "POST" }),
      )).json();
      assert.equal(connected.connected, true);
      assert.deepEqual(connected.tools, [{ name: "echo", description: "the echo tool" }]);
      assert.deepEqual(connected.connections.map((c: { state: string }) => c.state), ["connected"]);

      // Revoking takes the connection with it — a switched-off server must not keep
      // running with the user's credentials.
      const disabled = await (await call(
        new Request(`http://x/mcp/servers/echo/disable`, {
          method: "POST",
          body: JSON.stringify({ sessionId: session.id }),
        }),
      )).json();
      assert.deepEqual(disabled.active, []);
      assert.deepEqual(disabled.connections, []);

      // Removing the entry is a 404 the second time, and says so plainly.
      assert.equal(
        (await call(new Request(`http://x/mcp/servers/echo`, { method: "DELETE" }))).status,
        200,
      );
      const gone = await call(new Request(`http://x/mcp/servers/echo`, { method: "DELETE" }));
      assert.equal(gone.status, 404);
      assert.match((await gone.json()).error, /nothing to remove/);

      // A connect for a server nobody registered names the alternatives.
      const missing = await call(
        new Request(`http://x/mcp/servers/echo/connect${q}`, { method: "POST" }),
      );
      assert.equal(missing.status, 404);
      assert.match((await missing.json()).error, /No servers are registered yet/);
    } finally {
      await setMcpManager(previous).dropAll();
      db.close();
    }
  });
});

// ---------------------------------------------------------------------------
// The bridge, as boot wires it
// ---------------------------------------------------------------------------

test("a real program calls mcp() and mcpStatus() through the worker bridge", async () => {
  const file = tmpRegistry();
  seedRegistry(file);
  const mgr = manager(file);
  const workspace = mkdtempSync(join(tmpdir(), "bough-mcp-ws-"));
  const ctx = turnCtx("s1", { workspace });
  try {
    setActivation("s1", "echo", true, { file });
    // Exactly what `server/main.ts` bridges: the always-wired host functions, plus
    // the two MCP verbs over a ctx whose grant was bound for inheritance.
    const host = {
      ...baseHostFns(ctx),
      ...createMcpHostFns(bindTurnGrant(ctx, { file }), {
        manager: mgr,
        config: { file },
        auth: () => false,
      }),
    };
    const result = await runProgram({
      host,
      code: `
        const before = await mcpStatus();
        const said = await mcp("echo", "echo", { text: "through the bridge" });
        console.log(JSON.stringify({
          granted: before.active,
          registered: Object.keys(before.registry.servers),
          said,
          after: (await mcpStatus()).connections.map((c) => c.state),
        }));
      `,
    });
    assert.equal(result.ok, true, result.error);
    const seen = JSON.parse(result.logs.join(""));
    assert.deepEqual(seen.granted, ["echo"], "the program sees its grant as data");
    assert.deepEqual(seen.registered, ["echo"]);
    // The tool's structured content arrives as a real object, not a JSON string —
    // the worker re-inflates what the bridge stringified.
    assert.deepEqual(seen.said, { echoed: "through the bridge" });
    assert.deepEqual(seen.after, ["connected"]);
  } finally {
    await mgr.dropAll();
    rmSync(workspace, { recursive: true });
  }
});
