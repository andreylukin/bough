/**
 * The four delegation verbs, driven end to end against a real database, a real bus,
 * the real turn runner and the real launch path — with a scripted fake `LlmClient`
 * and a fake program runner standing in for the network and the worker (plan §7).
 * Nothing here spawns a process, binds a socket or needs a key.
 *
 * The interesting tests are the two containment ones, and they are deliberately two.
 * "An interrupt propagates to blocking children only" is not a single assertion: the
 * spawning turn's own `signal` must reach a blocking child and must NOT reach a
 * detached one, and an explicit stop of the spawner session must reach both — that
 * second path is the registry cascade hook (`turn/queue.ts`), and it is the only
 * stop a runaway detached child has. Testing one without the other would let either
 * half regress silently: drop the signal listener and a cancelled turn keeps burning
 * a branch; put a detached child on the signal and `spawn()` stops being detached.
 *
 * Timing is driven by gates, never by sleeps: `gatedLlm` parks the child's round
 * until the test releases it, so "the spawn returned before the child finished" is
 * an assertion about a child that is *provably* still running rather than about a
 * race that usually goes the right way.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable here, and a test that cannot run offline does not belong in
 * `deno task test`.
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { AgentError } from "../errors.ts";
import type { ProgramResult } from "../harness/protocol.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Session } from "../schema/parts.ts";
import type { LlmBlock, LlmClient, TurnCtx } from "../types.ts";
import type { SubagentResult } from "../agents/subagent.ts";
import { SpawnCaps } from "../agents/caps.ts";
import { TurnRegistry } from "../turn/queue.ts";
import { BASE_HOST_FNS, type ProgramRunner, STOP, type TurnDeps } from "../turn/runner.ts";
import {
  childTierOf,
  createDelegatingTurnStarter,
  createDelegationHostFns,
  type DelegationDeps,
  delegationFnsFor,
  delegationTier,
  delegationTurnDeps,
  DetachedSubagents,
  NESTED_DELEGATION,
  TOP_LEVEL_DELEGATION,
} from "./delegate.ts";

// ---- fixtures ---------------------------------------------------------------

interface Harness {
  db: SqliteDb;
  bus: Bus;
  events: BoughEvent[];
  registry: TurnRegistry;
  detached: DetachedSubagents;
  /** Own ledger per test: the process one is shared, and its budgets are per-turn. */
  caps: SpawnCaps;
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
    detached: new DetachedSubagents(),
    caps: new SpawnCaps(),
    close: () => db.close(),
  };
}

/** A program runner that never spawns a worker. */
const fakeProgram: ProgramRunner = () => Promise.resolve({ ok: true, logs: [] } as ProgramResult);

/** One round of text plus `stop` — the shortest complete turn there is. */
function reportingLlm(report: string): LlmClient {
  let round = 0;
  return {
    run() {
      round++;
      const content: LlmBlock[] = [
        { type: "text", text: report },
        { type: "tool_use", id: `stop-${round}`, name: STOP, input: {} },
      ];
      return Promise.resolve({ content, stopReason: "tool_use" });
    },
  };
}

interface GatedLlm {
  client: LlmClient;
  /** Resolves once the child's first round is actually in flight. */
  started: Promise<void>;
  /** Let the round finish. */
  release(): void;
}

/**
 * A model whose round parks until the test releases it — and that answers an
 * interrupt the way a real provider client does, by rejecting with an `AbortError`.
 * That is what makes an interrupted child land as `status: "interrupted"` rather
 * than as a turn that quietly succeeded after the stop.
 */
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
          { type: "tool_use", id: "stop-1", name: STOP, input: {} },
        ] as LlmBlock[],
        stopReason: "tool_use",
      };
    },
  };
  return { client, started, release };
}

interface Seeded {
  session: Session;
  supervisor: Message;
}

/** A session with an in-flight supervisor message, as a spawning turn would have. */
function seedSession(
  h: Harness,
  opts: { kind?: Session["kind"]; originId?: string; title?: string } = {},
): Seeded {
  const session = h.db.createSession({
    id: crypto.randomUUID(),
    title: opts.title ?? "the spawner",
    kind: opts.kind ?? "root",
    createdAt: 1_000,
    parentId: null,
    ...(opts.originId ? { originId: opts.originId } : {}),
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
  });
  const supervisor = h.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "supervisor",
    parts: [],
    pending: true,
    createdAt: 1_002,
  });
  return { session, supervisor };
}

/** The spawning turn's ctx, as the runner would have built it. */
function spawnerCtx(
  h: Harness,
  seeded: Seeded,
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
    workspace: "/tmp/checkout",
    model: "claude-test-model",
    signal: new AbortController().signal,
    depth: 0,
    ...extra,
  };
}

/** Child-turn deps that never touch a worker and never share the global registry. */
function childTurnDeps(h: Harness, extra: TurnDeps = {}): TurnDeps {
  return { registry: h.registry, program: fakeProgram, ...extra };
}

/** The delegation deps every test shares: own registry, own register, no worker. */
function delegationDeps(h: Harness, extra: DelegationDeps = {}): DelegationDeps {
  return {
    registry: h.registry,
    detached: h.detached,
    caps: h.caps,
    child: () => ({ turn: childTurnDeps(h) }),
    ...extra,
  };
}

/** The JSON a delegation verb hands back to the program, re-inflated. */
function parse(json: string): Record<string, unknown> {
  return JSON.parse(json) as Record<string, unknown>;
}

// ---- the blocking round trip -------------------------------------------------

Deno.test("agent() runs a child to completion and returns its report in-band", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const ctx = spawnerCtx(h, seeded, reportingLlm("Renamed foo to bar; tests pass."));
    const host = createDelegationHostFns(
      ctx,
      delegationDeps(h, {
        child: () => ({ turn: childTurnDeps(h), changedFiles: () => ["src/thing.ts"] }),
      }),
    );

    const result = parse(
      await host.agent!("Rename foo to bar in src/thing.ts.", JSON.stringify({ name: "renamer" })),
    );

    // Spec §6's four fields, and the report is the child's own final text.
    assert.equal(typeof result.sessionId, "string");
    assert.equal(result.ok, true);
    assert.equal(result.report, "Renamed foo to bar; tests pass.");
    assert.deepEqual(result.changedFiles, ["src/thing.ts"]);
    // The done-gate is gone (spec §17): there is no harness-verified check, so there
    // is no field that could claim one passed.
    assert.ok(!("checkPassed" in result), "no acceptance gate, and no field implying one");
    // Carried alongside `ok` so "failed" is not one undifferentiated fact.
    assert.equal(result.status, "done");
    assert.equal(result.title, "renamer", "the spawner's name labels the branch");

    // It really was a subagent branch of this session, and it really finished.
    const child = h.db.getSession(result.sessionId as string)!;
    assert.equal(child.kind, "subagent");
    assert.equal(child.originId, seeded.session.id);
    assert.equal(child.originMessageId, seeded.supervisor.id);
    assert.equal(h.db.threadFor(child.id).length, 2, "its task and its own answer, nothing else");
    assert.equal(h.registry.isRunning(child.id), false);
    // Blocking work leaves nothing detached behind for a later join() to find.
    assert.equal(h.detached.size, 0);
  } finally {
    h.close();
  }
});

Deno.test("a blocking child that fails reports why, without throwing at the spawner", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const llm: LlmClient = { run: () => Promise.reject(new Error("provider is on fire")) };
    const ctx = spawnerCtx(h, seeded, llm);
    const host = createDelegationHostFns(
      ctx,
      delegationDeps(h, {
        child: () => ({
          turn: childTurnDeps(h, { maxRoundRetries: 0, reportError: () => {} }),
        }),
      }),
    );

    const result = parse(await host.agent!("Do the impossible.", '{"name":"doomed"}'));
    assert.equal(result.ok, false);
    assert.equal(result.status, "error", "distinguishable from an interrupt or an orphan");
    assert.match(result.report as string, /on fire/);
  } finally {
    h.close();
  }
});

Deno.test("agent() refuses options it cannot use, naming the shape it wants", async () => {
  const h = harness();
  try {
    const ctx = spawnerCtx(h, seedSession(h), reportingLlm("x"));
    const host = createDelegationHostFns(ctx, delegationDeps(h));

    await assert.rejects(() => host.agent!("do it", '{"name":42}'), (err: unknown) => {
      assert.ok(err instanceof AgentError);
      assert.match(err.message, /name/);
      assert.match(err.message, /always pass a name/);
      return true;
    });
    await assert.rejects(() => host.agent!("do it", "not json"), (err: unknown) => {
      assert.ok(err instanceof AgentError);
      assert.match(err.message, /could not be read as JSON/);
      return true;
    });
    assert.equal(h.db.listSessions().length, 1, "no branch was created for a refused launch");
  } finally {
    h.close();
  }
});

Deno.test("a launch refused at a cap fails alone, naming the cap", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const ctx = spawnerCtx(h, seeded, reportingLlm("done"));
    // One launch per turn, so the second is refused without waiting for eight.
    const host = createDelegationHostFns(
      ctx,
      delegationDeps(h, { caps: new SpawnCaps({ perTurn: 1 }) }),
    );

    const first = parse(await host.agent!("The one launch this turn gets.", '{"name":"first"}'));
    assert.equal(first.ok, true);

    await assert.rejects(
      () => host.agent!("One too many.", '{"name":"second"}'),
      (err: unknown) => {
        assert.match((err as Error).message, /per-turn limit/);
        return true;
      },
    );
    // The refusal cost the sibling nothing: its branch and its report still stand.
    assert.equal(h.db.getSession(first.sessionId as string)!.outcomeOk, true);
    assert.equal(h.db.sessionsByOrigin(seeded.session.id).length, 1, "and no branch was created");
  } finally {
    h.close();
  }
});

// ---- detaching ---------------------------------------------------------------

Deno.test("spawn() returns before the child finishes, and the child runs on", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const gated = gatedLlm("swept the handlers");
    const ctx = spawnerCtx(h, seeded, gated.client);
    const delivered: SubagentResult[] = [];
    const host = createDelegationHostFns(
      ctx,
      delegationDeps(h, { deliver: (_c, r) => delivered.push(r) }),
    );

    const handle = parse(await host.spawn!("Sweep the handlers.", '{"name":"sweeper"}'));
    assert.equal(typeof handle.sessionId, "string");
    assert.equal(handle.title, "sweeper");
    assert.ok(!("report" in handle), "the handle is a promise of work, not its result");

    // The claim this test exists for: spawn() answered while the child's first round
    // is still in flight. Gated, so this is a fact and not a race that usually wins.
    const childId = handle.sessionId as string;
    await gated.started;
    assert.equal(h.registry.isRunning(childId), true, "the child is still mid-turn");
    assert.equal(delivered.length, 0, "and nothing has been reported yet");

    // The spawner's turn ends. A detached child is not part of it, so nothing about
    // the turn ending touches the child.
    assert.equal(h.registry.isRunning(childId), true);

    gated.release();
    const result = await h.detached.get(childId)!.result;
    assert.equal(result.ok, true);
    assert.equal(result.report, "swept the handlers");
    // Unclaimed, so the report is handed to the note deliverer (T4.4) rather than
    // being dropped.
    assert.deepEqual(delivered.map((r) => r.sessionId), [childId]);
  } finally {
    h.close();
  }
});

Deno.test("join() claims a detached child's result in-band, so no note is owed", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const gated = gatedLlm("audit complete: two missing error paths");
    const ctx = spawnerCtx(h, seeded, gated.client);
    const delivered: SubagentResult[] = [];
    const host = createDelegationHostFns(
      ctx,
      delegationDeps(h, { deliver: (_c, r) => delivered.push(r) }),
    );

    const handle = parse(await host.spawn!("Audit the handlers.", '{"name":"auditor"}'));
    const childId = handle.sessionId as string;
    await gated.started;

    const claimed = host.join!(childId);
    gated.release();
    const result = parse(await claimed);

    assert.equal(result.sessionId, childId);
    assert.equal(result.ok, true);
    assert.equal(result.report, "audit complete: two missing error paths");
    assert.equal(h.detached.get(childId)!.claimed, true);

    // Give the completion chain its microtasks: the note must NOT be posted, because
    // the program already has the report in hand.
    await h.detached.get(childId)!.result;
    await new Promise((r) => setTimeout(r, 0));
    assert.deepEqual(delivered, [], "a claimed result is not also announced as a note");

    // Joining again is a program being careful, not a program being wrong.
    assert.equal(parse(await host.join!(childId)).sessionId, childId);
  } finally {
    h.close();
  }
});

Deno.test("join() refuses an id this session never detached, and says what join is for", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const ctx = spawnerCtx(h, seeded, reportingLlm("x"));
    const host = createDelegationHostFns(ctx, delegationDeps(h));

    await assert.rejects(() => host.join!("no-such-session"), (err: unknown) => {
      assert.ok(err instanceof AgentError);
      assert.match(err.message, /has not spawn\(\)ed any/);
      assert.match(err.message, /restart clears it/, "says why an id can go missing");
      return true;
    });

    // A child of a DIFFERENT session is not joinable here either.
    const other = seedSession(h, { title: "someone else" });
    const otherHost = createDelegationHostFns(
      spawnerCtx(h, other, reportingLlm("done")),
      delegationDeps(h),
    );
    const theirs = parse(await otherHost.spawn!("Their work.", '{"name":"theirs"}'));
    await h.detached.get(theirs.sessionId as string)!.result;
    await assert.rejects(
      () => host.join!(theirs.sessionId as string),
      (err: unknown) => err instanceof AgentError,
    );
  } finally {
    h.close();
  }
});

// ---- containment -------------------------------------------------------------

Deno.test("the spawning turn's interrupt reaches a blocking child, not a detached one", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const blocking = gatedLlm("never gets here");
    const detachedLlm = gatedLlm("finished on my own");
    const turn = new AbortController();

    // Two ctxs over one session, so the two children get different scripted models
    // while sharing the spawning turn's signal — the thing under test.
    const blockingCtx = spawnerCtx(h, seeded, blocking.client, { signal: turn.signal });
    const detachedCtx = spawnerCtx(h, seeded, detachedLlm.client, { signal: turn.signal });
    const blockingHost = createDelegationHostFns(blockingCtx, delegationDeps(h));
    const detachedHost = createDelegationHostFns(detachedCtx, delegationDeps(h));

    const handle = parse(await detachedHost.spawn!("Long detached work.", '{"name":"detached"}'));
    const detachedId = handle.sessionId as string;
    const pending = blockingHost.agent!("Blocking work.", '{"name":"blocking"}');
    await blocking.started;
    await detachedLlm.started;

    const blockingId = h.db.sessionsByOrigin(seeded.session.id)
      .map((s) => s.id)
      .find((id) => id !== detachedId)!;
    assert.equal(h.registry.isRunning(blockingId), true);
    assert.equal(h.registry.isRunning(detachedId), true);

    // The user stops the spawning turn.
    turn.abort();
    const result = parse(await pending);

    assert.equal(result.status, "interrupted", "the blocking child is this turn's work");
    assert.equal(result.ok, false);
    assert.equal(h.db.getSession(blockingId)!.outcomeOk, false);

    // …and the detached one is not. It is still running, and it still finishes.
    assert.equal(
      h.registry.isRunning(detachedId),
      true,
      "a detached child survives its spawner's turn being interrupted",
    );
    detachedLlm.release();
    const survivor = await h.detached.get(detachedId)!.result;
    assert.equal(survivor.status, "done");
    assert.equal(survivor.report, "finished on my own");
  } finally {
    h.close();
  }
});

Deno.test("an explicit stop of the spawner session does cascade to a detached child", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const gated = gatedLlm("never finishes");
    const ctx = spawnerCtx(h, seeded, gated.client);
    const host = createDelegationHostFns(ctx, delegationDeps(h));

    const handle = parse(await host.spawn!("Runaway work.", '{"name":"runaway"}'));
    const childId = handle.sessionId as string;
    await gated.started;
    assert.equal(h.registry.isRunning(childId), true);

    // The registry cascade, which is a detached child's only stop path from above
    // (`turn/queue.ts`). The spawner's own turn is not even running here — hooks
    // fire regardless, because a detached child outlives the turn that started it.
    h.registry.interrupt(seeded.session.id);

    const result = await h.detached.get(childId)!.result;
    assert.equal(result.status, "interrupted");
    assert.equal(result.ok, false);

    // And the hook unregisters itself once the child has settled: a second stop
    // after the child is gone is a no-op rather than a throw out of the fan-out.
    await new Promise((r) => setTimeout(r, 0));
    assert.equal(h.registry.interrupt(seeded.session.id), false);
  } finally {
    h.close();
  }
});

Deno.test("a verb called after the turn was interrupted refuses instead of branching", async () => {
  const h = harness();
  try {
    const turn = new AbortController();
    turn.abort();
    const ctx = spawnerCtx(h, seedSession(h), reportingLlm("x"), { signal: turn.signal });
    const host = createDelegationHostFns(ctx, delegationDeps(h));

    const calls = [() => host.agent!("do it", "{}"), () => host.spawn!("do it", "{}")];
    for (const call of calls) {
      await assert.rejects(call, (err: unknown) => {
        assert.ok(err instanceof AgentError);
        assert.match(err.message, /interrupted/);
        return true;
      });
    }
    assert.equal(h.db.listSessions().length, 1, "no branch was created");
  } finally {
    h.close();
  }
});

// ---- adopt -------------------------------------------------------------------

Deno.test("adopt() validates the lineage and says there is nothing to merge", async () => {
  const h = harness();
  try {
    const seeded = seedSession(h);
    const ctx = spawnerCtx(h, seeded, reportingLlm("done"));
    const host = createDelegationHostFns(ctx, delegationDeps(h));

    const result = parse(await host.agent!("Do the thing.", '{"name":"worker"}'));
    const childId = result.sessionId as string;

    const before = h.events.length;
    const text = await host.adopt!(childId);
    assert.match(text, /worker/);
    assert.match(text, /nothing to\s+merge/);
    assert.match(text, /finished/);
    assert.ok(
      h.events.slice(before).some((e) => e.type === "session.updated" && e.sessionId === childId),
      "the branch is re-announced so the rail and the Changes view refresh",
    );

    // A session that is not this one's subagent is not adoptable.
    await assert.rejects(() => host.adopt!(seeded.session.id), (err: unknown) => {
      assert.ok(err instanceof AgentError);
      assert.match(err.message, /not a subagent of this session/);
      return true;
    });
  } finally {
    h.close();
  }
});

// ---- tiers -------------------------------------------------------------------

Deno.test("the tier follows the lineage, and the bridge follows the tier", () => {
  const h = harness();
  try {
    const root = seedSession(h);
    const one = seedSession(h, { kind: "subagent", originId: root.session.id, title: "level 1" });
    const two = seedSession(h, { kind: "subagent", originId: one.session.id, title: "level 2" });
    const wf = seedSession(h, { kind: "workflow_agent", title: "a workflow agent" });

    assert.equal(delegationTier(h.db, root.session.id), "top");
    assert.equal(delegationTier(h.db, one.session.id), "nested");
    assert.equal(delegationTier(h.db, two.session.id), "none", "the nesting cap, spec §7");
    assert.equal(delegationTier(h.db, wf.session.id), "none");
    assert.equal(delegationTier(h.db, "no such session"), "none");

    const bridged = (seeded: Seeded) =>
      Object.keys(
        createDelegationHostFns(spawnerCtx(h, seeded, reportingLlm("x")), delegationDeps(h)),
      ).sort();

    assert.deepEqual(bridged(root), ["adopt", "agent", "join", "spawn"]);
    assert.deepEqual(bridged(one), ["adopt", "agent"], "a subagent delegates blocking only");
    assert.deepEqual(
      bridged(two),
      [],
      "absence is the denial — the bridge rejects with the prompt's own wording",
    );
    assert.deepEqual(bridged(wf), []);

    assert.equal(childTierOf("top"), "nested");
    assert.equal(childTierOf("nested"), "none");
  } finally {
    h.close();
  }
});

Deno.test("each tier's grant matches what it can actually call", () => {
  // The prompt gate and the bridge are built from one list, per tier, so a section
  // documenting spawn() cannot reach a session that has no spawn() (spec §6).
  const granted = (tier: Parameters<typeof delegationTurnDeps>[0]) =>
    delegationTurnDeps(tier).granted!;

  assert.deepEqual(granted("top"), [...BASE_HOST_FNS, ...TOP_LEVEL_DELEGATION]);
  assert.deepEqual(granted("nested"), [...BASE_HOST_FNS, ...NESTED_DELEGATION]);
  assert.deepEqual(granted("none"), [...BASE_HOST_FNS]);
  for (const tier of ["top", "nested", "none"] as const) {
    assert.deepEqual(
      granted(tier).filter((fn) => (["agent", "spawn", "join", "adopt"] as string[]).includes(fn)),
      [...delegationFnsFor(tier)],
    );
  }
});

Deno.test("the wired starter picks the tier from the session it is starting", async () => {
  const h = harness();
  try {
    const root = seedSession(h, { title: "a root" });
    const started: { sessionId: string; granted: string[] }[] = [];
    // `assemble` is the runner's prompt seam: what it is handed IS the grant this
    // turn resolved, which is what makes the starter's tier choice observable.
    const start = createDelegatingTurnStarter({
      base: {
        registry: h.registry,
        program: fakeProgram,
        assemble: (input) => {
          started.push({ sessionId: "", granted: [...input.granted] });
          return { system: "", systemVolatile: "", sections: [] };
        },
      },
    });

    const ctx = { db: h.db, bus: h.bus, llm: reportingLlm("hello") };
    // A user message so the turn has something to answer.
    h.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: root.session.id,
      role: "user",
      parts: [{ type: "text", text: "hi" }],
      pending: false,
      createdAt: 2_000,
    });
    start(ctx, root.session, root.supervisor);
    // The turn is detached; wait for it to release the session.
    while (h.registry.isRunning(root.session.id)) await new Promise((r) => setTimeout(r, 1));

    assert.equal(started.length, 1);
    assert.ok(started[0].granted.includes("spawn"), "a root is granted detached delegation");

    // The same starter, a subagent session: blocking only.
    const sub = seedSession(h, {
      kind: "subagent",
      originId: root.session.id,
      title: "a subagent",
    });
    h.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: sub.session.id,
      role: "user",
      parts: [{ type: "text", text: "do it" }],
      pending: false,
      createdAt: 2_100,
    });
    start(ctx, sub.session, sub.supervisor);
    while (h.registry.isRunning(sub.session.id)) await new Promise((r) => setTimeout(r, 1));

    assert.equal(started.length, 2);
    assert.ok(started[1].granted.includes("agent"));
    assert.ok(!started[1].granted.includes("spawn"), "a subagent may not detach work");
  } finally {
    h.close();
  }
});
