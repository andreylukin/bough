/**
 * The wake rule, driven against a real database, a real bus and the real turn
 * runner — with a scripted fake `LlmClient` and a fake program runner standing in
 * for the network and the worker (plan §7). Nothing here binds a socket or needs a
 * key; the one test that touches a process runs `/bin/sh -c 'exit 3'`.
 *
 * The load-bearing assertion is not in any single test: it is `watchTurns`, which
 * subscribes to the bus for the whole of every test and records a violation if a
 * session ever has two turns open at once. Every test asserts it saw none — so a
 * change that let a note start a turn on a busy session, or that let a queued drain
 * race the turn it queued behind, fails wherever it happens rather than only in the
 * one test written for it.
 *
 * Timing comes from gates and from `until`, never from fixed sleeps: "the note
 * landed while a turn was provably still running" is the distinction under test, and
 * a sleep would make it a coin flip.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable here, and a test that cannot run offline does not belong in
 * `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { SpawnCapError } from "../errors.ts";
import type { ProgramResult } from "../harness/protocol.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { AppCtx, LlmClient, LlmParams, LlmResult, TurnCtx } from "../types.ts";
import { SpawnCaps } from "./caps.ts";
import { JobRegistry } from "../hostfn/jobs.ts";
import { createDelegationHostFns, DetachedSubagents } from "../hostfn/delegate.ts";
import { TurnRegistry } from "../turn/queue.ts";
import {
  beginTurn,
  createTurnStarter,
  type ProgramRunner,
  RUN_STEPS,
  STOP,
  type TurnDeps,
} from "../turn/runner.ts";
import { recoverOrphanedTurns } from "../turn/state.ts";
import type { SubagentResult } from "./subagent.ts";
import {
  createJobNotifier,
  createNoteDeliverer,
  deliverSubagentNote,
  formatSubagentNote,
  type NoteDelivery,
  noteOrphanedSubagents,
  postSystemNote,
  SUBAGENT_NOTE_PREFIX,
} from "./notes.ts";

// ---- fixtures ---------------------------------------------------------------

/** Poll until `pred` holds. Bounded, so a broken wake fails as a timeout, not a hang. */
async function until(pred: () => boolean, what: string, ms = 5_000): Promise<void> {
  const deadline = Date.now() + ms;
  while (!pred()) {
    if (Date.now() > deadline) throw new Error(`timed out waiting for: ${what}`);
    await new Promise((r) => setTimeout(r, 1));
  }
}

/**
 * One turn per session, watched from the bus.
 *
 * A turn announces itself with a `message.started` carrying its pending supervisor
 * message and closes with `turn.finished`, so the depth between the two is the
 * number of turns a session has open. Anything above one is the invariant broken.
 */
interface TurnWatch {
  /** Session ids in the order their turns began. */
  starts: string[];
  /** Recorded whenever a session's open-turn depth exceeded one. */
  violations: string[];
  turnsFor(sessionId: string): number;
}

function watchTurns(bus: Bus): TurnWatch {
  const live = new Map<string, number>();
  const starts: string[] = [];
  const violations: string[] = [];
  bus.subscribe((e: BoughEvent) => {
    const id = e.sessionId;
    if (!id) return;
    if (e.type === "message.started" && (e.data as Message).role === "supervisor") {
      const depth = (live.get(id) ?? 0) + 1;
      live.set(id, depth);
      starts.push(id);
      if (depth > 1) violations.push(id);
    } else if (e.type === "turn.finished") {
      live.set(id, Math.max(0, (live.get(id) ?? 0) - 1));
    }
  });
  return {
    starts,
    violations,
    turnsFor: (sessionId) => starts.filter((s) => s === sessionId).length,
  };
}

interface Harness {
  db: SqliteDb;
  bus: Bus;
  registry: TurnRegistry;
  detached: DetachedSubagents;
  caps: SpawnCaps;
  watch: TurnWatch;
  close(): void;
}

function harness(caps: SpawnCaps = new SpawnCaps()): Harness {
  const db = openDb(":memory:");
  const bus = new Bus();
  return {
    db,
    bus,
    registry: new TurnRegistry(),
    detached: new DetachedSubagents(),
    caps,
    watch: watchTurns(bus),
    close: () => db.close(),
  };
}

/** A program runner that never spawns a worker. */
const fakeProgram: ProgramRunner = () => Promise.resolve({ ok: true, logs: [] } as ProgramResult);

/** A root session with one user message — idle, with no turn ever run. */
function seedSpawner(h: Harness, title = "the spawner"): Session {
  const session = h.db.createSession({
    id: crypto.randomUUID(),
    title,
    kind: "root",
    createdAt: 1_000,
    parentId: null,
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
  });
  h.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "user",
    parts: [{ type: "text", text: "delegate the audit" }],
    pending: false,
    createdAt: 1_001,
  });
  return session;
}

/** The detached child's briefing. Distinctive, because the dispatcher routes on it. */
const CHILD_TASK = "CHILD-BRIEF: audit every handler for missing error paths";

interface ScriptedLlm {
  client: LlmClient;
  /** Deep snapshots — the runner mutates its `messages` array between rounds. */
  calls: LlmParams[];
}

/**
 * The spawner's model: one program, then a report.
 *
 * The "am I done" test is the presence of a tool result or of a harness note in the
 * thread, never a round counter — the woken turn is a *different* turn with its own
 * round numbering, and what the test cares about is that the note reached the
 * provider payload at all.
 */
function spawnerLlm(): ScriptedLlm {
  const calls: LlmParams[] = [];
  const client: LlmClient = {
    run(params) {
      calls.push(structuredClone(params));
      const payload = JSON.stringify(params.messages);
      const done = payload.includes('"tool_result"') ||
        payload.includes(SUBAGENT_NOTE_PREFIX) ||
        payload.includes("[background]");
      return Promise.resolve({
        content: done
          ? [
            { type: "text", text: "acknowledged" },
            { type: "tool_use", id: `stop-${calls.length}`, name: STOP, input: {} },
          ]
          : [{
            type: "tool_use",
            id: `call-${calls.length}`,
            name: RUN_STEPS,
            // The task text stays out of the program, so the dispatcher below can
            // tell a spawner round from a child round by the thread alone.
            input: { code: "await spawn(task)" },
          }],
        stopReason: "tool_use",
      } as LlmResult);
    },
  };
  return { client, calls };
}

interface GatedLlm {
  client: LlmClient;
  /** Resolves once the round is actually in flight. */
  started: Promise<void>;
  release(): void;
}

/** A model whose round parks until released, and that rejects like a real abort. */
function gatedLlm(report: string): GatedLlm {
  let release!: () => void;
  const gate = new Promise<void>((r) => (release = r));
  let markStarted!: () => void;
  const started = new Promise<void>((r) => (markStarted = r));
  const client: LlmClient = {
    async run(_params, _onText, signal) {
      markStarted();
      await new Promise<void>((resolve, reject) => {
        const onAbort = () => reject(new DOMException("interrupted", "AbortError"));
        if (signal?.aborted) return onAbort();
        signal?.addEventListener("abort", onAbort, { once: true });
        gate.then(() => {
          signal?.removeEventListener("abort", onAbort);
          resolve();
        });
      });
      return {
        content: [
          { type: "text", text: report },
          { type: "tool_use", id: "stop-gated", name: STOP, input: {} },
        ],
        stopReason: "tool_use",
      } as LlmResult;
    },
  };
  return { client, started, release };
}

/**
 * One client, two actors.
 *
 * A subagent inherits its spawner's `ctx.llm` (that is the point of `childCtx` in
 * `agents/subagent.ts`), so a test cannot hand the child a different client — it
 * routes by thread instead. A child's thread is exactly its task; the spawner's
 * never contains it.
 */
function twoActorLlm(child: LlmClient, spawner: LlmClient): LlmClient {
  return {
    run(params, onText, signal) {
      const payload = JSON.stringify(params.messages);
      return payload.includes(CHILD_TASK)
        ? child.run(params, onText, signal)
        : spawner.run(params, onText, signal);
    },
  };
}

/** A result as `buildResult` would have assembled it, for the formatting tests. */
function result(over: Partial<SubagentResult> = {}): SubagentResult {
  return {
    sessionId: "child-1",
    title: "seatbelt audit",
    ok: true,
    status: "done",
    report: "Checked every handler; two were missing error paths.",
    changedFiles: [],
    ...over,
  };
}

/** The spawner's app ctx, carrying the wake seam boot assigns (`ctx.startTurn`). */
function spawnerCtx(h: Harness, llm: LlmClient, turnDeps: TurnDeps): AppCtx {
  const ctx: AppCtx & { startTurn?: unknown } = { db: h.db, bus: h.bus, llm };
  ctx.startTurn = createTurnStarter(turnDeps);
  return ctx as AppCtx;
}

/**
 * Turn deps whose program detaches one child through the REAL delegation verb, with
 * the note deliverer wired the way boot wires it.
 */
function delegatingDeps(
  h: Harness,
  opts: { deliveries: NoteDelivery[]; childTimeoutMs?: number } = { deliveries: [] },
): TurnDeps {
  return {
    registry: h.registry,
    reportError: () => {},
    programFor: (turnCtx: TurnCtx) => {
      const host = createDelegationHostFns(turnCtx, {
        registry: h.registry,
        detached: h.detached,
        caps: h.caps,
        reportError: () => {},
        child: () => ({
          ...(opts.childTimeoutMs ? { timeoutMs: opts.childTimeoutMs } : {}),
          turn: {
            registry: h.registry,
            program: fakeProgram,
            reportError: () => {},
            maxRoundRetries: 0,
          },
        }),
        deliver: (ctx, r) => {
          opts.deliveries.push(deliverSubagentNote(ctx, r, { registry: h.registry }));
        },
      });
      return async () => {
        const handle = JSON.parse(await host.spawn!(CHILD_TASK, "{}"));
        return { ok: true, logs: [`spawned: ${handle.sessionId}`] } as ProgramResult;
      };
    },
  };
}

// ---- the note itself --------------------------------------------------------

test("the four subagent outcomes read differently in the note", () => {
  const done = formatSubagentNote(result());
  assert.ok(done.startsWith(`${SUBAGENT_NOTE_PREFIX} "seatbelt audit" (child-1) — finished.`));
  assert.match(done, /Report:\nChecked every handler/);
  // The most common wrong move after a delegated report is looking for the merge.
  assert.match(done, /already here/);
  assert.match(done, /nothing to merge/);

  const errored = formatSubagentNote(result({ ok: false, status: "error" }));
  const stopped = formatSubagentNote(result({ ok: false, status: "interrupted" }));
  const orphaned = formatSubagentNote(result({ ok: false, status: "orphaned" }));

  assert.match(errored, /FAILED — its turn errored/);
  assert.match(stopped, /STOPPED — it was interrupted/);
  assert.match(orphaned, /ORPHANED — the server restarted/);

  // Distinguishable is the requirement, not merely "not ok": four outcomes, four
  // different first lines.
  const heads = [done, errored, stopped, orphaned].map((n) => n.split("\n")[0]);
  assert.equal(new Set(heads).size, 4);

  // Each failure says what survived — a subagent shares the checkout, so partial
  // work is already on disk and a parent told only "failed" will redo it.
  for (const note of [errored, stopped, orphaned]) {
    assert.match(note, /already written is in the checkout/);
  }

  assert.match(formatSubagentNote(result()), /Changed files: not reported\./);
  assert.match(
    formatSubagentNote(result({ changedFiles: ["a.ts", "b.ts"] })),
    /Changed files: a\.ts, b\.ts\./,
  );
});

// ---- wake path 1: the idle spawner ------------------------------------------

test("a detached child that finishes while its spawner is idle starts a fresh turn", async () => {
  const h = harness();
  try {
    const session = seedSpawner(h);
    const child = gatedLlm("the audit found two gaps");
    const spawner = spawnerLlm();
    const deliveries: NoteDelivery[] = [];

    const deps = delegatingDeps(h, { deliveries });
    const ctx = spawnerCtx(h, twoActorLlm(child.client, spawner.client), deps);

    const outcome = await beginTurn(ctx, session.id, deps).done;

    // The spawner is done and idle; the detached child is provably still running.
    // That is the state the wake rule is about, and it is asserted rather than
    // assumed because a child that had already finished would test nothing.
    assert.equal(outcome.status, "done");
    assert.equal(h.registry.isRunning(session.id), false);
    const childId = h.detached.idsFor(session.id)[0];
    assert.ok(childId, "the program detached a child");
    assert.equal(h.registry.isRunning(childId), true, "which outlived its spawner's turn");
    assert.equal(h.watch.turnsFor(session.id), 1, "nothing has been woken yet");

    child.release();
    await until(() => deliveries.length === 1, "the child's report to be delivered");
    assert.equal(deliveries[0].wake, "started", "an idle spawner is woken with a fresh turn");

    await until(() => h.registry.isRunning(session.id) === false, "the woken turn to finish");
    assert.equal(h.watch.turnsFor(session.id), 2, "exactly one fresh turn");

    // The note is in the spawner's own thread, as a system message…
    const note = h.db.messagesFor(session.id).find((m) => m.role === "system");
    assert.ok(note, "the report landed as a system-role message");
    assert.match((note.parts[0] as { text: string }).text, /^\[subagent finished\]/);
    assert.match((note.parts[0] as { text: string }).text, /the audit found two gaps/);

    // …and it reached the model, which is the only reason to wake at all.
    assert.match(JSON.stringify(spawner.calls.at(-1)!.messages), /subagent finished/);

    assert.deepEqual(h.watch.violations, [], "no session ever ran two turns at once");
  } finally {
    h.close();
  }
});

// ---- wake path 2: the busy spawner ------------------------------------------

test("a note that lands mid-turn rides the queued drain instead of racing it", async () => {
  const h = harness();
  try {
    const session = seedSpawner(h);
    const gated = gatedLlm("first turn's answer");
    const deps: TurnDeps = { registry: h.registry, program: fakeProgram, reportError: () => {} };
    const ctx = spawnerCtx(h, gated.client, deps);

    const first = beginTurn(ctx, session.id, deps);
    await gated.started;
    assert.equal(h.registry.isRunning(session.id), true, "a turn is provably in flight");

    const delivery = postSystemNote(
      ctx,
      session.id,
      formatSubagentNote(result({ sessionId: "child-9" })),
      { registry: h.registry },
    );
    assert.equal(delivery.wake, "queued", "a busy session is never given a second turn");
    assert.equal(h.watch.turnsFor(session.id), 1, "and none was started behind its back");
    // Persisted and announced immediately all the same: the UI shows the report the
    // instant it happens, whatever the model does about it.
    assert.ok(delivery.message);
    assert.equal(h.db.getMessage(delivery.message.id)?.role, "system");

    gated.release();
    await first.done;

    // The drain: the running turn ends and the note it queued behind becomes the
    // next turn — one, not one per note and not none.
    await until(() => h.watch.turnsFor(session.id) === 2, "the queued drain to start a turn");
    await until(() => h.registry.isRunning(session.id) === false, "the drained turn to finish");
    assert.equal(h.watch.turnsFor(session.id), 2);

    const roles = h.db.messagesFor(session.id).map((m) => m.role);
    assert.deepEqual(
      roles,
      ["user", "supervisor", "system", "supervisor"],
      "ordered, nothing lost",
    );
    assert.deepEqual(h.watch.violations, []);
  } finally {
    h.close();
  }
});

test("a burst of notes on a busy session drains into exactly one turn", async () => {
  const h = harness();
  try {
    const session = seedSpawner(h);
    const gated = gatedLlm("first turn's answer");
    const deps: TurnDeps = { registry: h.registry, program: fakeProgram, reportError: () => {} };
    const ctx = spawnerCtx(h, gated.client, deps);

    const first = beginTurn(ctx, session.id, deps);
    await gated.started;

    // Four children finishing at once is the ordinary shape of a fan-out, not an
    // edge case: `Promise.allSettled` over four detached spawns ends exactly here.
    const wakes = [1, 2, 3, 4].map((n) =>
      postSystemNote(ctx, session.id, `${SUBAGENT_NOTE_PREFIX} child ${n}`, {
        registry: h.registry,
      }).wake
    );
    assert.deepEqual(wakes, ["queued", "queued", "queued", "queued"]);

    gated.release();
    await first.done;
    await until(() => h.watch.turnsFor(session.id) === 2, "one drained turn");
    await until(() => h.registry.isRunning(session.id) === false, "it to finish");

    assert.equal(h.watch.turnsFor(session.id), 2, "four notes, one turn — not four");
    assert.equal(h.db.messagesFor(session.id).filter((m) => m.role === "system").length, 4);
    assert.deepEqual(h.watch.violations, []);
  } finally {
    h.close();
  }
});

test("a burst of notes on an IDLE session also produces exactly one turn", async () => {
  const h = harness();
  try {
    const session = seedSpawner(h);
    const spawner = spawnerLlm();
    const deps: TurnDeps = { registry: h.registry, program: fakeProgram, reportError: () => {} };
    const ctx = spawnerCtx(h, spawner.client, deps);

    // The first note finds the session idle and starts a turn SYNCHRONOUSLY — the
    // registry is claimed inside `beginTurn` before it returns — so the second and
    // third already see a busy session. That is why two notes in one tick cannot
    // both start a turn.
    const wakes = [1, 2, 3].map((n) =>
      postSystemNote(ctx, session.id, `${SUBAGENT_NOTE_PREFIX} child ${n}`, {
        registry: h.registry,
      }).wake
    );
    assert.deepEqual(wakes, ["started", "queued", "queued"]);

    await until(() => h.watch.turnsFor(session.id) === 2, "the drain for the two queued notes");
    await until(() => h.registry.isRunning(session.id) === false, "the drained turn to finish");

    assert.equal(h.watch.turnsFor(session.id), 2);
    assert.deepEqual(h.watch.violations, [], "no session ever ran two turns at once");
  } finally {
    h.close();
  }
});

// ---- the two deliberate non-wakes -------------------------------------------

test("a stop stays stopped: a note into a session the user interrupted wakes nothing", () => {
  const h = harness();
  try {
    const session = seedSpawner(h);
    // The session's last turn ended because the user stopped it — which is also
    // what cascade-stopped the detached child whose note is arriving now.
    const message = h.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: session.id,
      role: "supervisor",
      parts: [{ type: "text", text: "⏹ Stopped." }],
      pending: false,
      createdAt: 1_002,
    });
    h.db.createTurn({
      id: crypto.randomUUID(),
      sessionId: session.id,
      messageId: message.id,
      status: "interrupted",
      step: "ended",
      createdAt: 1_002,
      updatedAt: 1_003,
      error: null,
    });

    const deps: TurnDeps = { registry: h.registry, program: fakeProgram };
    const ctx = spawnerCtx(h, spawnerLlm().client, deps);
    const delivery = postSystemNote(
      ctx,
      session.id,
      formatSubagentNote(result({ ok: false, status: "interrupted" })),
      { registry: h.registry },
    );

    assert.equal(delivery.wake, "recorded", "the stop is still in force");
    assert.ok(delivery.message, "but the report is still written into the thread");
    assert.equal(h.watch.turnsFor(session.id), 0, "nothing restarted the work the user stopped");
    assert.deepEqual(h.watch.violations, []);
  } finally {
    h.close();
  }
});

test("a note for a session that is gone is dropped, never thrown", () => {
  const h = harness();
  try {
    const deps: TurnDeps = { registry: h.registry, program: fakeProgram };
    const ctx = spawnerCtx(h, spawnerLlm().client, deps);
    // Every caller is a completion callback with nowhere to report a failure, and a
    // throw there is an unhandled rejection that takes the process down.
    const delivery = postSystemNote(ctx, "no-such-session", `${SUBAGENT_NOTE_PREFIX} ghost`, {
      registry: h.registry,
    });
    assert.deepEqual(delivery, { message: null, wake: "dropped" });
  } finally {
    h.close();
  }
});

// ---- the failure matrix (plan T4.4) -----------------------------------------

test("failure matrix: a detached child that ERRORED reaches its spawner as FAILED", async () => {
  const h = harness();
  try {
    const session = seedSpawner(h);
    const spawner = spawnerLlm();
    const onFire: LlmClient = { run: () => Promise.reject(new Error("provider is on fire")) };
    const deliveries: NoteDelivery[] = [];

    const deps = delegatingDeps(h, { deliveries });
    const ctx = spawnerCtx(h, twoActorLlm(onFire, spawner.client), deps);

    await beginTurn(ctx, session.id, deps).done;
    await until(() => deliveries.length === 1, "the failed child's note");

    const text = (deliveries[0].message!.parts[0] as { text: string }).text;
    assert.match(text, /FAILED — its turn errored/);
    assert.match(text, /on fire/, "the report carries the actual error");
    assert.doesNotMatch(text, /STOPPED|ORPHANED/);

    await until(() => h.registry.isRunning(session.id) === false, "the woken turn to finish");
    assert.deepEqual(h.watch.violations, []);
  } finally {
    h.close();
  }
});

test("failure matrix: a detached child STOPPED by its wall clock says so, and wakes", async () => {
  const h = harness();
  try {
    const session = seedSpawner(h);
    const spawner = spawnerLlm();
    const child = gatedLlm("never gets here");
    const deliveries: NoteDelivery[] = [];

    // A timeout, not a user stop: the spawner's own turn ended cleanly, so this note
    // SHOULD wake it — which is exactly what separates it from the stop above.
    const deps = delegatingDeps(h, { deliveries, childTimeoutMs: 20 });
    const ctx = spawnerCtx(h, twoActorLlm(child.client, spawner.client), deps);

    await beginTurn(ctx, session.id, deps).done;
    await until(() => deliveries.length === 1, "the timed-out child's note");

    const text = (deliveries[0].message!.parts[0] as { text: string }).text;
    assert.match(text, /STOPPED — it was interrupted/);
    assert.match(text, /wall-clock limit/, "and names a cause the parent can act on");
    assert.equal(deliveries[0].wake, "started");

    await until(() => h.registry.isRunning(session.id) === false, "the woken turn to finish");
    assert.deepEqual(h.watch.violations, []);
  } finally {
    h.close();
  }
});

test("failure matrix: a launch REFUSED at the cap is in-band and owes no note", async () => {
  // One launch per turn, so the second is refused by the per-turn budget.
  const h = harness(new SpawnCaps({ perTurn: 1 }));
  try {
    const session = seedSpawner(h);
    const before = h.db.listSessions().length;
    const deliveries: NoteDelivery[] = [];

    const ctx: TurnCtx = {
      db: h.db,
      bus: h.bus,
      llm: spawnerLlm().client,
      sessionId: session.id,
      turnId: "turn-1",
      messageId: "message-1",
      workspace: "/tmp/checkout",
      model: "claude-test-model",
      signal: new AbortController().signal,
      depth: 0,
    };
    const host = createDelegationHostFns(ctx, {
      registry: h.registry,
      detached: h.detached,
      caps: h.caps,
      reportError: () => {},
      child: () => ({
        turn: { registry: h.registry, program: fakeProgram, reportError: () => {} },
      }),
      deliver: (c, r) => {
        deliveries.push(deliverSubagentNote(c, r, { registry: h.registry }));
      },
    });

    const first = JSON.parse(await host.spawn!(CHILD_TASK, "{}"));
    await assert.rejects(
      () => host.spawn!("the one over the cap", "{}"),
      (err: unknown) => {
        // A refusal is the launcher's answer to the program, in-band. Nothing ran,
        // so there is nothing to report to a later turn and no branch to report on.
        assert.ok(err instanceof SpawnCapError);
        assert.match(err.message, /cap/i);
        return true;
      },
    );

    await until(() => deliveries.length === 1, "the child that actually ran reporting");
    assert.equal(
      h.db.listSessions().length,
      before + 1,
      "the refused launch created no branch, so no note is owed for it",
    );
    assert.deepEqual(h.detached.idsFor(session.id), [first.sessionId as string]);
    assert.equal(h.db.messagesFor(session.id).filter((m) => m.role === "system").length, 1);
    assert.deepEqual(h.watch.violations, []);
  } finally {
    h.close();
  }
});

test("failure matrix: a child ORPHANED by a restart reaches its spawner without waking it", async () => {
  const h = harness();
  try {
    const spawner = seedSpawner(h);
    const spawnerMessage = h.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: spawner.id,
      role: "supervisor",
      parts: [{ type: "text", text: "spawning the audit" }],
      pending: false,
      createdAt: 1_002,
    });
    // What the previous process left behind: a subagent branch whose turn row still
    // says `running`, and a detached register that died with it.
    const child = h.db.createSession({
      id: crypto.randomUUID(),
      title: "seatbelt audit",
      kind: "subagent",
      createdAt: 1_003,
      parentId: null,
      originId: spawner.id,
      originMessageId: spawnerMessage.id,
      workspace: "/tmp/checkout",
      originDir: "/tmp/checkout",
    });
    const childMessage = h.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: child.id,
      role: "supervisor",
      parts: [],
      pending: true,
      createdAt: 1_004,
    });
    h.db.createTurn({
      id: crypto.randomUUID(),
      sessionId: child.id,
      messageId: childMessage.id,
      status: "running",
      step: "round:2",
      createdAt: 1_004,
      updatedAt: 1_005,
      error: null,
    });

    const deps: TurnDeps = { registry: h.registry, program: fakeProgram };
    const ctx = spawnerCtx(h, spawnerLlm().client, deps);

    const orphans = recoverOrphanedTurns(h.db, h.bus);
    const posted = await noteOrphanedSubagents(ctx, orphans, { registry: h.registry });

    assert.equal(posted.length, 1, "the stranded child owes its spawner exactly one note");
    const note = posted[0];
    assert.equal(note.message!.sessionId, spawner.id, "posted to the SPAWNER, not the child");
    const text = (note.message!.parts[0] as { text: string }).text;
    assert.match(text, /ORPHANED — the server restarted/);
    assert.ok(text.includes(child.id), "and names the branch, so the user can open it");

    // Recorded, not woken: `turn/state.ts` surfaces a restart rather than resuming
    // it, and a server coming back must not start spending on its own.
    assert.equal(note.wake, "recorded");
    assert.equal(h.watch.turnsFor(spawner.id), 0);
    assert.equal(h.registry.isRunning(spawner.id), false);
    assert.deepEqual(h.watch.violations, []);
  } finally {
    h.close();
  }
});

// ---- background jobs post the same way --------------------------------------

test("a background job's exit posts through the same wake rule", async () => {
  const h = harness();
  const jobs = new JobRegistry();
  try {
    const session = seedSpawner(h);
    const spawner = spawnerLlm();
    const deps: TurnDeps = { registry: h.registry, program: fakeProgram, reportError: () => {} };
    const ctx = spawnerCtx(h, spawner.client, deps);

    // The wiring boot performs: the process-wide job registry exists before the
    // thing that knows how to post a note, so the notifier is attached afterwards.
    jobs.attachBus(h.bus);
    jobs.attachNotifier(createJobNotifier(ctx, { registry: h.registry }));

    // A non-zero exit always notifies. (A silent clean exit deliberately does not —
    // it would wake an idle session into a whole turn to say nothing.)
    jobs.bashBg("failing job", "exit 3", { sessionId: session.id, workspace: process.cwd() });

    await until(
      () => h.db.messagesFor(session.id).some((m) => m.role === "system"),
      "the job exit note",
    );
    const note = h.db.messagesFor(session.id).find((m) => m.role === "system")!;
    assert.match(
      (note.parts[0] as { text: string }).text,
      /^\[background\] bg_1 "failing job" finished \(exit 3/,
    );

    // And it woke the idle session exactly once — the same rule the subagent note
    // obeys, because both go through `postSystemNote`.
    await until(() => h.watch.turnsFor(session.id) === 1, "the woken turn");
    await until(() => h.registry.isRunning(session.id) === false, "the woken turn to finish");
    assert.equal(h.watch.turnsFor(session.id), 1);
    assert.deepEqual(h.watch.violations, []);
  } finally {
    jobs.killAll();
    await jobs.drain();
    h.close();
  }
});

// ---- the production seam ----------------------------------------------------

test("createNoteDeliverer is the deliver seam hostfn/delegate.ts takes", async () => {
  const h = harness();
  try {
    const session = seedSpawner(h);
    const spawner = spawnerLlm();
    const deps: TurnDeps = { registry: h.registry, program: fakeProgram, reportError: () => {} };
    const ctx = spawnerCtx(h, spawner.client, deps);
    const deliver = createNoteDeliverer({ registry: h.registry });

    // The shape `delegationTurnDeps` passes it: a turn ctx and the child's result.
    deliver({ ...ctx, sessionId: session.id } as TurnCtx, result({ title: "the audit" }));

    const note = h.db.messagesFor(session.id).find((m) => m.role === "system");
    assert.ok(note, "the seam posts into the spawner's session");
    assert.match((note.parts[0] as { text: string }).text, /\[subagent finished\] "the audit"/);

    await until(() => h.registry.isRunning(session.id) === false, "the woken turn to finish");
    assert.equal(h.watch.turnsFor(session.id), 1);
    assert.deepEqual(h.watch.violations, []);
  } finally {
    h.close();
  }
});
