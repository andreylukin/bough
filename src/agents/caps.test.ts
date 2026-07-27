/**
 * The width caps, driven the way a program drives them: many launches fired at
 * once through `Promise.allSettled`, with the ledger inspected afterwards.
 *
 * Every test here is about ACCOUNTING, so none of them launches a real subagent —
 * a fake launch is `{sessionId, result}` and nothing else, which is exactly the
 * surface `underLease` guards. That keeps the caps testable with no turn runner, no
 * provider and no worker, and it keeps a failure here readable as what it is: a
 * counting bug, not a delegation bug.
 *
 * The two headline scenarios are deliberately different shapes, because the two
 * caps can only bind in different shapes:
 *
 *   - Twelve launches whose children finish before the next one starts never have
 *     two running at once, so the PER-TURN cap is what refuses the last four.
 *   - Twelve launches that all stay in flight exhaust the tree-wide budget on the
 *     fourth, so CONCURRENCY is what refuses the other eight.
 *
 * Both assert the same invariant from opposite sides: the refusals leave the
 * successes untouched, and take nothing for themselves.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is
 * not reachable here, and a test that cannot run offline does not belong in
 * `deno task test` (plan §7).
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import { AgentError, SpawnCapError } from "../errors.ts";
import type { Session } from "../schema/parts.ts";
import type { TurnCtx } from "../types.ts";
import {
  assertMayDelegate,
  cappedLaunch,
  exemptLease,
  type LeasedLaunch,
  MAX_SPAWNS_PER_TURN,
  MAX_TREE_CONCURRENT,
  reserveSpawn,
  SpawnCaps,
  type SpawnLease,
  treeRootOf,
  underLease,
} from "./caps.ts";

// ---- fixtures ---------------------------------------------------------------

/** The slice of `TurnCtx` the caps read. Nothing else is needed to count. */
type CapsCtx = Pick<TurnCtx, "db" | "sessionId" | "turnId" | "depth">;

interface Harness {
  db: SqliteDb;
  close(): void;
}

function harness(): Harness {
  const db = openDb(":memory:");
  return { db, close: () => db.close() };
}

let seq = 0;

/** A session of any kind, optionally hung off a lineage edge. */
function seed(
  h: Harness,
  opts: { kind?: Session["kind"]; originId?: string } = {},
): Session {
  return h.db.createSession({
    id: `s${++seq}-${crypto.randomUUID().slice(0, 8)}`,
    title: opts.kind ?? "root",
    kind: opts.kind ?? "root",
    createdAt: 1_000 + seq,
    parentId: null,
    ...(opts.originId ? { originId: opts.originId, originMessageId: `m-${opts.originId}` } : {}),
    workspace: "/tmp/checkout",
    originDir: "/tmp/checkout",
  });
}

function ctxFor(h: Harness, session: Session, turnId: string, depth = 0): CapsCtx {
  return { db: h.db, sessionId: session.id, turnId, depth };
}

/** A launch whose child has already finished — the slot comes straight back. */
function instantLaunch(id: string): LeasedLaunch {
  return { sessionId: id, result: Promise.resolve(`report from ${id}`) };
}

/** A launch whose child is still running until the test says otherwise. */
function pendingLaunch(id: string): LeasedLaunch & { finish: () => void } {
  let finish!: () => void;
  const result = new Promise<void>((resolve) => {
    finish = resolve;
  });
  return { sessionId: id, result, finish };
}

const isCapError = (err: unknown): err is SpawnCapError => err instanceof SpawnCapError;

// ---- the acceptance criterion ------------------------------------------------

Deno.test("allSettled over twelve launches: eight succeed, four are refused, the eight stand", async () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const root = seed(h);
    const ctx = ctxFor(h, root, "turn-1");

    // Each launch waits for the previous one's child to finish, so at most one is
    // ever in flight and the tree-wide budget never binds — this scenario is about
    // the per-turn cap. They are still twelve concurrent promises under one
    // `allSettled`, which is the shape a fan-out program actually writes.
    let gate: Promise<unknown> = Promise.resolve();
    const attempts = Array.from({ length: 12 }, (_, i) => {
      const attempt = gate.then(async () => {
        const launch = cappedLaunch(ctx, { mode: "blocking", caps }, () => instantLaunch(`c${i}`));
        return await launch.result;
      });
      gate = attempt.catch(() => {});
      return attempt;
    });

    const settled = await Promise.allSettled(attempts);
    const fulfilled = settled.filter((s) => s.status === "fulfilled");
    const rejected = settled.filter((s) => s.status === "rejected");

    assert.equal(fulfilled.length, MAX_SPAWNS_PER_TURN, "eight launches went through");
    assert.equal(rejected.length, 12 - MAX_SPAWNS_PER_TURN, "four were refused");

    // The successes are intact: the FIRST eight, each with its own child's own
    // report. A refusal that had unwound a sibling would show up here as a hole.
    assert.deepEqual(
      settled.slice(0, 8).map((s) => (s.status === "fulfilled" ? s.value : s.reason)),
      Array.from({ length: 8 }, (_, i) => `report from c${i}`),
    );
    assert.ok(
      settled.slice(8).every((s) => s.status === "rejected" && isCapError(s.reason)),
      "the refusals are SpawnCapErrors, and they are the LAST four",
    );

    for (const r of rejected) {
      const err = (r as PromiseRejectedResult).reason as SpawnCapError;
      assert.equal(err.status, 429);
      assert.match(err.message, /per-turn limit \(8\)/, "the error names WHICH cap");
      assert.match(err.message, /workflow/, "and the move that resolves it");
    }

    assert.equal(caps.spawnedInTurn("turn-1"), 8, "only the launches that happened were charged");
    assert.equal(caps.running(), 0, "every slot came back when its child finished");
  } finally {
    h.close();
  }
});

Deno.test("twelve launches that all stay running: four take the tree, eight are refused", async () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const root = seed(h);
    const ctx = ctxFor(h, root, "turn-1");
    const started: ReturnType<typeof pendingLaunch>[] = [];

    // Fired in one synchronous burst: every reservation happens before any child
    // could possibly report. This is the case a database query cannot get right.
    const settled = await Promise.allSettled(
      Array.from({ length: 12 }, (_, i) => {
        // Not an `async` wrapper: a cap refusal is a SYNCHRONOUS throw, and this
        // keeps all twelve reservations inside one tick rather than spreading them
        // over twelve microtasks, which is the point of the test.
        try {
          const launch = cappedLaunch(ctx, { mode: "detached", caps }, () => {
            const l = pendingLaunch(`c${i}`);
            started.push(l);
            return l;
          });
          return Promise.resolve(launch.sessionId);
        } catch (err) {
          return Promise.reject(err);
        }
      }),
    );

    assert.equal(settled.filter((s) => s.status === "fulfilled").length, MAX_TREE_CONCURRENT);
    assert.equal(settled.filter((s) => s.status === "rejected").length, 12 - MAX_TREE_CONCURRENT);
    assert.deepEqual(
      settled.slice(0, 4).map((s) => (s.status === "fulfilled" ? s.value : s.reason)),
      ["c0", "c1", "c2", "c3"],
    );
    assert.equal(started.length, 4, "a refused launch never ran the launch body at all");

    const refusal = (settled[4] as PromiseRejectedResult).reason as SpawnCapError;
    assert.ok(isCapError(refusal));
    assert.match(refusal.message, /concurrency cap reached/);
    assert.match(refusal.message, /tree-wide limit \(4\)/, "the error names WHICH cap");
    assert.match(refusal.message, /join\(\)/, "and the move that resolves it");

    // The four in flight are untouched by the eight refusals.
    assert.equal(caps.running(root.id), 4);
    assert.equal(caps.spawnedInTurn("turn-1"), 4, "refusals charged nothing to the turn");

    for (const l of started) l.finish();
    await Promise.all(started.map((l) => l.result));
    await Promise.resolve();
    assert.equal(caps.running(), 0, "the slots come back as the children finish");
  } finally {
    h.close();
  }
});

Deno.test("a refused launch releases nothing it did not take", async () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const root = seed(h);
    const ctx = ctxFor(h, root, "turn-1");

    const held = Array.from({ length: 4 }, (_, i) => {
      const launch = pendingLaunch(`held-${i}`);
      return { launch, ...cappedLaunch(ctx, { mode: "detached", caps }, () => launch) };
    });
    assert.equal(caps.running(root.id), 4);

    for (let i = 0; i < 3; i++) {
      assert.throws(
        () => cappedLaunch(ctx, { mode: "detached", caps }, () => instantLaunch(`refused-${i}`)),
        SpawnCapError,
      );
    }

    // Neither budget moved: the refusals did not free a sibling's slot, and they
    // did not spend the turn's per-turn allowance either.
    assert.equal(caps.running(root.id), 4, "the four in flight still hold their slots");
    assert.equal(caps.spawnedInTurn("turn-1"), 4);

    for (const h2 of held) h2.launch.finish();
    await Promise.all(held.map((h2) => h2.launch.result));
    await Promise.resolve();
    assert.equal(caps.running(root.id), 0);

    // Four more still fit, because the three refusals cost the turn nothing — the
    // per-turn budget is eight LAUNCHES, not eight attempts.
    for (let i = 0; i < 4; i++) {
      cappedLaunch(ctx, { mode: "blocking", caps }, () => instantLaunch(`later-${i}`));
    }
    assert.equal(caps.spawnedInTurn("turn-1"), 8);
    assert.throws(
      () => cappedLaunch(ctx, { mode: "blocking", caps }, () => instantLaunch("ninth")),
      (err: unknown) => isCapError(err) && /per-turn limit/.test(err.message),
    );
  } finally {
    h.close();
  }
});

// ---- the tree-wide counter ---------------------------------------------------

Deno.test("the concurrency budget is the TREE's, not the session's", () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const root = seed(h);
    const child = seed(h, { kind: "subagent", originId: root.id });
    const grandchild = seed(h, { kind: "subagent", originId: child.id });

    assert.equal(treeRootOf(h.db, root.id), root.id);
    assert.equal(treeRootOf(h.db, child.id), root.id);
    assert.equal(treeRootOf(h.db, grandchild.id), root.id, "every hop lands on the same tree");

    // Three DIFFERENT sessions, three different turns, one budget.
    const fromRoot = ctxFor(h, root, "turn-root");
    const fromChild = ctxFor(h, child, "turn-child", 1);
    const fromGrandchild = ctxFor(h, grandchild, "turn-grandchild", 1);

    reserveSpawn(fromRoot, { mode: "detached", caps });
    reserveSpawn(fromRoot, { mode: "detached", caps });
    reserveSpawn(fromChild, { mode: "blocking", caps });
    reserveSpawn(fromGrandchild, { mode: "blocking", caps });

    assert.equal(caps.running(root.id), 4);
    assert.equal(caps.spawnedInTurn("turn-root"), 2, "per-turn counts stay per turn");
    assert.equal(caps.spawnedInTurn("turn-child"), 1);

    // The fifth is refused wherever in the tree it is launched from — including
    // from a session that has launched nothing at all.
    for (const c of [fromRoot, fromChild, fromGrandchild]) {
      assert.throws(
        () => reserveSpawn(c, { mode: "blocking", caps }),
        (err: unknown) => isCapError(err) && /tree-wide limit/.test(err.message),
      );
    }

    // A different tree is different work, and holds its own budget.
    const other = seed(h);
    const lease = reserveSpawn(ctxFor(h, other, "turn-other"), { mode: "detached", caps });
    assert.equal(caps.running(other.id), 1);
    assert.equal(caps.running(), 5, "five running overall, four of them in one tree");
    lease.release();
    assert.equal(caps.running(other.id), 0);
    assert.equal(caps.running(root.id), 4, "releasing one tree's slot never touches another's");
  } finally {
    h.close();
  }
});

Deno.test("a fork is its own tree, and a dangling origin does not hang the walk", () => {
  const h = harness();
  try {
    const root = seed(h);
    const fork = seed(h, { kind: "fork", originId: root.id });
    assert.equal(treeRootOf(h.db, fork.id), fork.id, "a fork is a branch, not a delegation");

    const orphan = seed(h, { kind: "subagent", originId: "gone-with-the-database" });
    assert.equal(treeRootOf(h.db, orphan.id), orphan.id);

    // A ten-deep subagent chain still resolves, and resolves to the top.
    let cur = root;
    for (let i = 0; i < 10; i++) cur = seed(h, { kind: "subagent", originId: cur.id });
    assert.equal(treeRootOf(h.db, cur.id), root.id);
  } finally {
    h.close();
  }
});

// ---- releasing --------------------------------------------------------------

Deno.test("releasing twice frees one slot, not two", () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const root = seed(h);
    const ctx = ctxFor(h, root, "turn-1");

    const first = reserveSpawn(ctx, { mode: "detached", caps });
    reserveSpawn(ctx, { mode: "detached", caps });
    assert.equal(caps.running(root.id), 2);

    first.release();
    first.release();
    first.release();
    assert.equal(first.released, true);
    assert.equal(caps.running(root.id), 1, "the second lease still holds its slot");
  } finally {
    h.close();
  }
});

Deno.test("a launch that throws releases the slot it reserved", () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const root = seed(h);
    const ctx = ctxFor(h, root, "turn-1");

    assert.throws(
      () =>
        cappedLaunch(ctx, { mode: "detached", caps }, () => {
          throw new AgentError(400, "task must be a non-empty string");
        }),
      AgentError,
    );

    assert.equal(caps.running(root.id), 0, "nothing is running, so nothing is held");
    // The per-turn budget IS charged: the model asked for a launch and the answer
    // it gets back is about its own bad call, not about a cap. Eight more still fit
    // only if the failure is not counted, so assert what we actually do.
    assert.equal(caps.spawnedInTurn("turn-1"), 1);
  } finally {
    h.close();
  }
});

// ---- the bus backstop -------------------------------------------------------

Deno.test("a dropped lease is released when the child's turn finishes", async () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const bus = new Bus();
    const detach = caps.attachBus(bus);
    const root = seed(h);
    const child = seed(h, { kind: "subagent", originId: root.id });
    const ctx = ctxFor(h, root, "turn-1");

    // A launch whose result nobody will ever settle — a detached child the holder
    // forgot about. Without the backstop this slot is gone for the process's life.
    const launch = cappedLaunch(ctx, { mode: "detached", caps }, () => ({
      sessionId: child.id,
      result: new Promise<void>(() => {}),
    }));
    assert.equal(caps.running(root.id), 1);
    assert.equal(launch.sessionId, child.id);

    // An unrelated session's turn finishing must not free it.
    bus.publish({
      type: "turn.finished",
      sessionId: "someone-else",
      data: { turnId: "t-other", sessionId: "someone-else", status: "done" },
    });
    assert.equal(caps.running(root.id), 1);

    bus.publish({
      type: "turn.finished",
      sessionId: child.id,
      data: { turnId: "t-child", sessionId: child.id, status: "interrupted" },
    });
    assert.equal(caps.running(root.id), 0, "the child's turn ended, so its slot came back");

    // And the spawning turn's own end clears its per-turn tally.
    assert.equal(caps.spawnedInTurn("turn-1"), 1);
    bus.publish({
      type: "turn.finished",
      sessionId: root.id,
      data: { turnId: "turn-1", sessionId: root.id, status: "done" },
    });
    assert.equal(caps.spawnedInTurn("turn-1"), 0);

    detach();
    assert.equal(bus.size, 0, "attachBus hands back a working unsubscribe");
    await Promise.resolve();
  } finally {
    h.close();
  }
});

Deno.test("the bus backstop and the result path releasing together free one slot", async () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const bus = new Bus();
    const detach = caps.attachBus(bus);
    const root = seed(h);
    const kids = [
      seed(h, { kind: "subagent", originId: root.id }),
      seed(h, {
        kind: "subagent",
        originId: root.id,
      }),
    ];
    const ctx = ctxFor(h, root, "turn-1");

    const launches = kids.map((k) => {
      const pending = pendingLaunch(k.id);
      cappedLaunch(ctx, { mode: "detached", caps }, () => pending);
      return pending;
    });
    assert.equal(caps.running(root.id), 2);

    // Both release paths fire for the same child — exactly what happens in
    // production. Idempotence is what keeps this from freeing a slot nobody held.
    launches[0].finish();
    bus.publish({
      type: "turn.finished",
      sessionId: kids[0].id,
      data: { turnId: "t0", sessionId: kids[0].id, status: "done" },
    });
    await launches[0].result;
    await Promise.resolve();

    assert.equal(caps.running(root.id), 1, "one child ended, one slot back");
    launches[1].finish();
    await launches[1].result;
    await Promise.resolve();
    assert.equal(caps.running(root.id), 0);
    detach();
  } finally {
    h.close();
  }
});

// ---- the nesting rule -------------------------------------------------------

Deno.test("a subagent may delegate blocking, and is refused a detached spawn", () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const root = seed(h);
    const child = seed(h, { kind: "subagent", originId: root.id });

    // Top level: both modes are available.
    assertMayDelegate({ depth: 0 }, "detached");
    assertMayDelegate({ depth: 0 }, "blocking");
    // One level down: blocking only.
    assertMayDelegate({ depth: 1 }, "blocking");

    const nested = ctxFor(h, child, "turn-child", 1);
    let err: unknown;
    try {
      reserveSpawn(nested, { mode: "detached", verb: "spawn()", caps });
      assert.fail("a detached launch from inside a subagent must be refused");
    } catch (caught) {
      err = caught;
    }
    assert.ok(err instanceof AgentError);
    assert.ok(!(err instanceof SpawnCapError), "a nesting refusal is not a cap to retry later");
    assert.equal(err.status, 400);
    assert.match(err.message, /spawn\(\) is not available inside a subagent/);
    assert.match(err.message, /agent\(task, \{name\}\)/, "it names the verb that does work");

    assert.equal(caps.running(root.id), 0, "a refused nesting took no slot");
    assert.equal(caps.spawnedInTurn("turn-child"), 0, "and no per-turn budget");

    // The blocking form from the same session goes through.
    reserveSpawn(nested, { mode: "blocking", caps });
    assert.equal(caps.running(root.id), 1);
  } finally {
    h.close();
  }
});

// ---- the workflow exemption --------------------------------------------------

Deno.test("a workflow's launches are exempt from both caps", () => {
  const h = harness();
  try {
    const caps = new SpawnCaps();
    const root = seed(h);
    const ctx = ctxFor(h, root, "turn-1");

    const leases: SpawnLease[] = [];
    for (let i = 0; i < 20; i++) {
      leases.push(reserveSpawn(ctx, { mode: "blocking", exempt: true, caps }));
    }
    assert.equal(caps.running(root.id), 0, "an exempt launch is not in the ledger at all");
    assert.equal(caps.spawnedInTurn("turn-1"), 0);

    // The exempt lease is still a lease: bindable, releasable, idempotent.
    const lease = exemptLease();
    lease.bind("child-1");
    assert.equal(lease.sessionId, "child-1");
    lease.release();
    lease.release();
    assert.equal(lease.released, true);
  } finally {
    h.close();
  }
});

// ---- injectable limits -------------------------------------------------------

Deno.test("the caps are the spec's numbers, and are injectable for tests", () => {
  assert.equal(MAX_SPAWNS_PER_TURN, 8);
  assert.equal(MAX_TREE_CONCURRENT, 4);

  const caps = new SpawnCaps();
  assert.equal(caps.perTurn, 8);
  assert.equal(caps.concurrent, 4);

  const tiny = new SpawnCaps({ perTurn: 2, concurrent: 1 });
  const lease = tiny.reserve({ turnId: "t", treeId: "tree" });
  assert.throws(() => tiny.reserve({ turnId: "t", treeId: "tree" }), SpawnCapError);
  lease.release();
  tiny.reserve({ turnId: "t", treeId: "tree" });
  assert.throws(
    () => tiny.reserve({ turnId: "t", treeId: "tree" }),
    (err: unknown) => isCapError(err) && /per-turn limit \(2\)/.test(err.message),
  );

  tiny.reset();
  assert.equal(tiny.running(), 0);
  assert.equal(tiny.spawnedInTurn("t"), 0);
});

Deno.test("underLease binds the child session so the ledger can find the lease", async () => {
  const caps = new SpawnCaps();
  const lease = caps.reserve({ turnId: "t", treeId: "tree" });
  assert.equal(lease.sessionId, null);
  assert.equal(lease.treeId, "tree");
  assert.equal(lease.turnId, "t");

  const launch = underLease(lease, () => instantLaunch("kid"));
  assert.equal(lease.sessionId, "kid");
  await launch.result;
  await Promise.resolve();
  assert.equal(lease.released, true);
  assert.equal(caps.running("tree"), 0);
});
