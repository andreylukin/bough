/**
 * The launch path, driven end to end against a real database, a real bus and the
 * real turn runner — with a scripted fake `LlmClient` and a fake program runner in
 * place of the network and the worker (plan §7). Nothing here spawns a process,
 * binds a socket, or needs a key.
 *
 * The load-bearing test is the first one, and it makes the isolation claim twice
 * because one direction is not enough. Asserting the child's stored thread proves
 * what the database holds; asserting the messages the fake provider was actually
 * sent proves what the MODEL saw — and it is the second that the invariant is about.
 * A future change that reintroduced a parent pointer, or that seeded the child with
 * "context" for helpfulness, would still pass a thread-length check written loosely.
 * So the spawner's transcript carries a sentinel string, and the test asserts that
 * string never appears anywhere in the child's provider payload.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable here, and a test that cannot run offline does not belong in
 * `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { AgentError } from "../errors.ts";
import type { ProgramResult } from "../harness/protocol.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { LlmBlock, LlmClient, LlmParams, TurnCtx } from "../types.ts";
import { TurnRegistry } from "../turn/queue.ts";
import { type ProgramRunner, STOP, type TurnDeps } from "../turn/runner.ts";
import {
  cleanSubagentName,
  launchSubagent,
  MAX_SUBAGENT_DEPTH,
  subagentDepth,
  taskStubTitle,
  UNTITLED,
} from "./subagent.ts";

// ---- fixtures ---------------------------------------------------------------

/** A string that exists ONLY in the spawner's transcript. */
const SPAWNER_SECRET = "PINEAPPLE-QUADRANT-7";

/**
 * The visibility derivation, restated locally rather than imported from
 * `server/sessions.ts`. Two reasons, and the second is the real one: `agents/` has
 * no business importing from `server/`, and `server/sessions.ts` ↔ `server/app.ts`
 * is an import cycle that only resolves when `app.ts` is evaluated first — pulling
 * it in from here loads it the other way round and throws at module init.
 */
const isCollapsed = (s: Session): boolean => s.kind === "subagent" || s.kind === "workflow_agent";

interface FakeLlm {
  client: LlmClient;
  /** Deep snapshots — the runner mutates its `messages` array between rounds. */
  calls: LlmParams[];
}

/** One round of text plus `stop`, which is the shortest complete turn there is. */
function reportingLlm(report: string): FakeLlm {
  const calls: LlmParams[] = [];
  const client: LlmClient = {
    run(params) {
      calls.push(structuredClone(params));
      const content: LlmBlock[] = [
        { type: "text", text: report },
        { type: "tool_use", id: `stop-${calls.length}`, name: STOP, input: {} },
      ];
      return Promise.resolve({ content, stopReason: "tool_use" });
    },
  };
  return { client, calls };
}

/** A model that runs one program and then reports — used for the error path. */
function failingLlm(): FakeLlm {
  const calls: LlmParams[] = [];
  const client: LlmClient = {
    run(params) {
      calls.push(structuredClone(params));
      return Promise.reject(new Error("provider is on fire"));
    },
  };
  return { client, calls };
}

/** A program runner that never spawns a worker. */
function fakeProgram(logs: string[] = ["ok"]): ProgramRunner {
  return () => Promise.resolve({ ok: true, logs } satisfies ProgramResult);
}

interface Harness {
  db: SqliteDb;
  bus: Bus;
  events: BoughEvent[];
  registry: TurnRegistry;
  close(): void;
}

function harness(): Harness {
  const db = openDb(":memory:");
  const bus = new Bus();
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  return {
    db,
    bus,
    events,
    registry: new TurnRegistry(),
    close: () => db.close(),
  };
}

/** A spawner session with a transcript the child must not inherit. */
function seedSpawner(h: Harness, opts: { workspace?: string; kind?: Session["kind"] } = {}): {
  session: Session;
  supervisor: Message;
} {
  const session = h.db.createSession({
    id: crypto.randomUUID(),
    title: "the spawner",
    kind: opts.kind ?? "root",
    createdAt: 1_000,
    parentId: null,
    workspace: opts.workspace ?? "/tmp/checkout",
    originDir: opts.workspace ?? "/tmp/checkout",
  });
  h.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "user",
    parts: [{ type: "text", text: `the plan is ${SPAWNER_SECRET}, do not tell anyone` }],
    pending: false,
    createdAt: 1_001,
  });
  const supervisor = h.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "supervisor",
    parts: [{ type: "text", text: `acknowledged, ${SPAWNER_SECRET} it is` }],
    pending: true,
    createdAt: 1_002,
  });
  return { session, supervisor };
}

/** The spawning turn's ctx, as the runner would have built it. */
function spawnerCtx(
  h: Harness,
  seeded: { session: Session; supervisor: Message },
  llm: LlmClient,
  extra: Partial<TurnCtx> = {},
): TurnCtx {
  return {
    db: h.db,
    bus: h.bus,
    llm,
    sessionId: seeded.session.id,
    turnId: "turn-spawner",
    messageId: seeded.supervisor.id,
    workspace: seeded.session.workspace ?? "/tmp/checkout",
    model: "claude-test-model",
    signal: new AbortController().signal,
    depth: 0,
    ...extra,
  };
}

/** Child-turn deps that never touch a worker and never share the global registry. */
function childTurnDeps(h: Harness, extra: TurnDeps = {}): TurnDeps {
  return { registry: h.registry, program: fakeProgram(), ...extra };
}

// ---- the invariant ----------------------------------------------------------

test("a launched subagent's thread is its task and nothing else", async () => {
  const h = harness();
  try {
    const seeded = seedSpawner(h);
    const llm = reportingLlm("done: touched one file");
    const ctx = spawnerCtx(h, seeded, llm.client);
    const task = "Rename `foo` to `bar` in src/thing.ts and run the tests.";

    const launch = launchSubagent(ctx, task, {}, { turn: childTurnDeps(h) });

    // Snapshotted before the turn writes anything: at launch the child's whole
    // thread is the task. Asserted after the await, so a failure here reports as a
    // failed assertion rather than as a closed database under a still-running turn.
    const atLaunch = h.db.threadFor(launch.sessionId);

    await launch.result;

    // The task, and the empty supervisor placeholder `beginTurn` opened to answer
    // it. Nothing else — no ancestor's messages, because there is no ancestor.
    assert.deepEqual(atLaunch.map((m) => m.role), ["user", "supervisor"]);
    assert.ok(
      atLaunch.every((m) => m.sessionId === launch.sessionId),
      "every message in the thread is the child's own — nothing is inherited",
    );
    assert.equal(atLaunch[0].id, launch.taskMessage.id);
    assert.deepEqual(atLaunch[0].parts, [{ type: "text", text: task }]);
    assert.deepEqual(atLaunch[1].parts, [], "the placeholder is empty at launch");
    assert.equal(launch.session.parentId, null, "parentId null is what makes it task-only");

    // And what the MODEL saw: one user message, the task, nothing of the spawner's.
    assert.equal(llm.calls.length, 1, "one round: text plus stop");
    const sent = llm.calls[0].messages;
    assert.equal(sent.length, 1, "the child's first round carries only its briefing");
    assert.equal(sent[0].role, "user");
    assert.deepEqual(sent[0].content, [{ type: "text", text: task }]);

    // The strong form: no fragment of the spawner's conversation reached the wire.
    const payload = JSON.stringify(llm.calls[0]);
    assert.ok(
      !payload.includes(SPAWNER_SECRET),
      "the spawner's transcript must not leak into the child's payload",
    );
  } finally {
    h.close();
  }
});

test("lineage points back at the spawning turn", async () => {
  const h = harness();
  try {
    const seeded = seedSpawner(h);
    const llm = reportingLlm("done");
    const ctx = spawnerCtx(h, seeded, llm.client);

    const launch = launchSubagent(ctx, "Check the error paths in server/app.ts.", {}, {
      turn: childTurnDeps(h),
    });
    await launch.result;

    const child = h.db.getSession(launch.sessionId)!;
    assert.equal(child.kind, "subagent");
    assert.equal(child.originId, seeded.session.id, "originId is the spawning session");
    assert.equal(
      child.originMessageId,
      seeded.supervisor.id,
      "originMessageId is the supervisor message that was in flight",
    );

    // The edge is what makes it reachable: collapsed out of the top level, present
    // under its origin. Both halves, because either alone is a session nobody sees.
    assert.ok(isCollapsed(child), "subagents collapse under their origin");
    assert.deepEqual(
      h.db.sessionsByOrigin(seeded.session.id).map((s) => s.id),
      [child.id],
      "the drill-in finds it",
    );
    assert.ok(
      !h.db.listSessions().filter((s) => !isCollapsed(s)).some((s) => s.id === child.id),
      "the top-level listing does not",
    );
  } finally {
    h.close();
  }
});

// ---- naming -----------------------------------------------------------------

test("the name defaults to the task's first 40 characters", () => {
  assert.equal(taskStubTitle("Audit the handlers"), "Audit the handlers");
  assert.equal(taskStubTitle("  Audit  the\n rest of it "), "Audit the");

  const long = "Review every request handler in the server for missing error paths";
  const stub = taskStubTitle(long);
  assert.ok(stub.length <= 41, `"${stub}" fits the 40-char budget plus the ellipsis`);
  assert.ok(stub.endsWith("…"));
  assert.ok(long.startsWith(stub.slice(0, -1).trimEnd()), "it is a prefix of the task");
  // Word boundary, not a mid-word chop.
  assert.equal(stub, "Review every request handler in the…");

  // A single 60-character word has no boundary worth cutting at; a hard cut beats
  // throwing the whole budget away.
  assert.equal(taskStubTitle("x".repeat(60)), `${"x".repeat(40)}…`);
  assert.equal(taskStubTitle("   "), UNTITLED);
});

test("a spawner-supplied name wins, and is safe to render", () => {
  assert.equal(cleanSubagentName("audit the seatbelt profile"), "audit the seatbelt profile");
  assert.equal(cleanSubagentName("two\nlines\there"), "two lines here");
  assert.equal(cleanSubagentName("   "), undefined, "empty once cleaned falls back");
  assert.equal(cleanSubagentName(undefined), undefined);
  assert.equal(cleanSubagentName("y".repeat(80))?.length, 48);
  assert.throws(() => cleanSubagentName(42), AgentError);
});

test("the given name titles the branch; otherwise the task stub does", async () => {
  const h = harness();
  try {
    const seeded = seedSpawner(h);
    const llm = reportingLlm("done");
    const ctx = spawnerCtx(h, seeded, llm.client);

    const named = launchSubagent(
      ctx,
      "Some very long briefing that would otherwise become the title",
      { name: "seatbelt audit" },
      { turn: childTurnDeps(h) },
    );
    assert.equal(named.title, "seatbelt audit");
    assert.equal(h.db.getSession(named.sessionId)!.title, "seatbelt audit");
    await named.result;

    const unnamed = launchSubagent(ctx, "Fix the flaky test in db.test.ts", {}, {
      turn: childTurnDeps(h),
    });
    assert.equal(unnamed.title, "Fix the flaky test in db.test.ts");
    await unnamed.result;
  } finally {
    h.close();
  }
});

// ---- what else crosses the boundary -----------------------------------------

test("the child runs in the spawner's checkout and inherits its MCP grant", async () => {
  const h = harness();
  try {
    const seeded = seedSpawner(h, { workspace: "/tmp/shared-checkout" });
    const llm = reportingLlm("done");
    const ctx = spawnerCtx(h, seeded, llm.client, { mcpGrant: ["github", "linear"] });

    let childCtx: TurnCtx | undefined;
    const launch = launchSubagent(ctx, "Do the thing.", {}, {
      turn: childTurnDeps(h, {
        program: undefined,
        programFor: (c) => {
          childCtx = c;
          return fakeProgram();
        },
      }),
    });
    await launch.result;

    assert.equal(
      h.db.getSession(launch.sessionId)!.workspace,
      "/tmp/shared-checkout",
      "same checkout as the spawner — no worktree, nothing to merge",
    );
    assert.equal(childCtx?.workspace, "/tmp/shared-checkout");
    assert.deepEqual(childCtx?.mcpGrant, ["github", "linear"], "the grant carries into the child");
    assert.equal(childCtx?.model, "claude-test-model", "and so does the spawning turn's model");
    assert.equal(childCtx?.depth, 1, "the child is a delegated tier");
    assert.equal(
      h.db.getSession(launch.sessionId)!.model,
      null,
      "inherited, not pinned — a later manual continuation follows the global default",
    );
  } finally {
    h.close();
  }
});

test("the launch announces the branch before its first message", async () => {
  const h = harness();
  try {
    const seeded = seedSpawner(h);
    const llm = reportingLlm("done");
    const ctx = spawnerCtx(h, seeded, llm.client);

    const launch = launchSubagent(ctx, "Do the thing.", {}, { turn: childTurnDeps(h) });
    const forChild = h.events.filter((e) => e.sessionId === launch.sessionId);
    assert.equal(forChild[0].type, "session.created");
    assert.equal(forChild[1].type, "message.started");
    assert.equal((forChild[1].data as Message).id, launch.taskMessage.id);

    await launch.result;
    assert.ok(
      h.events.some((e) => e.sessionId === launch.sessionId && e.type === "session.updated"),
      "and announces the branch again when it finishes, so the rail can retire it",
    );
  } finally {
    h.close();
  }
});

// ---- the result -------------------------------------------------------------

test("the result carries the child's report and its outcome", async () => {
  const h = harness();
  try {
    const seeded = seedSpawner(h);
    const llm = reportingLlm("Renamed foo to bar in src/thing.ts; tests pass.");
    const ctx = spawnerCtx(h, seeded, llm.client);

    const launch = launchSubagent(ctx, "Rename foo to bar.", {}, {
      turn: childTurnDeps(h),
      changedFiles: () => ["src/thing.ts"],
    });
    const result = await launch.result;

    assert.equal(result.sessionId, launch.sessionId);
    assert.equal(result.status, "done");
    assert.equal(result.ok, true);
    assert.equal(result.report, "Renamed foo to bar in src/thing.ts; tests pass.");
    assert.deepEqual(result.changedFiles, ["src/thing.ts"]);
    assert.equal(h.db.getSession(launch.sessionId)!.outcomeOk, true);
  } finally {
    h.close();
  }
});

test("a child whose turn errored reports ok:false and says why", async () => {
  const h = harness();
  try {
    const seeded = seedSpawner(h);
    const ctx = spawnerCtx(h, seeded, failingLlm().client);

    const launch = launchSubagent(ctx, "Do the impossible.", {}, {
      turn: childTurnDeps(h, { maxRoundRetries: 0, reportError: () => {} }),
    });
    const result = await launch.result;

    assert.equal(result.ok, false);
    assert.equal(result.status, "error", "distinguishable from an interrupt or an orphan");
    assert.match(result.report, /on fire/);
    assert.equal(h.db.getSession(launch.sessionId)!.outcomeOk, false);
  } finally {
    h.close();
  }
});

// ---- refusals ---------------------------------------------------------------

test("an empty task is refused with a message that says what a task is for", () => {
  const h = harness();
  try {
    const seeded = seedSpawner(h);
    const ctx = spawnerCtx(h, seeded, reportingLlm("x").client);
    assert.throws(() => launchSubagent(ctx, "   "), (err: unknown) => {
      assert.ok(err instanceof AgentError);
      assert.match(err.message, /entire briefing/);
      return true;
    });
    assert.equal(h.db.listSessions().length, 1, "nothing was created");
  } finally {
    h.close();
  }
});

test("delegation stops at the depth cap", () => {
  const h = harness();
  try {
    // root → subagent(1) → subagent(2). The one at depth 2 may not delegate further.
    const root = seedSpawner(h);
    let originId = root.session.id;
    let deepest = root.session;
    for (let i = 0; i < MAX_SUBAGENT_DEPTH; i++) {
      deepest = h.db.createSession({
        id: crypto.randomUUID(),
        title: `level ${i + 1}`,
        kind: "subagent",
        createdAt: 2_000 + i,
        parentId: null,
        originId,
        originMessageId: root.supervisor.id,
        workspace: "/tmp/checkout",
      });
      originId = deepest.id;
    }
    assert.equal(subagentDepth(h.db, root.session.id), 0);
    assert.equal(subagentDepth(h.db, deepest.id), MAX_SUBAGENT_DEPTH);

    const ctx = spawnerCtx(
      h,
      { session: deepest, supervisor: root.supervisor },
      reportingLlm("x").client,
    );
    assert.throws(() => launchSubagent(ctx, "one level too far"), (err: unknown) => {
      assert.ok(err instanceof AgentError);
      assert.match(err.message, /depth limit \(2\)/);
      return true;
    });
  } finally {
    h.close();
  }
});
