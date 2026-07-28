/**
 * Tests for live activity blurbs.
 *
 * THE LOAD-BEARING ONE is "the drop rule holds under a burst": twelve `run_steps`
 * rounds landing on one session while a blurb is in flight buy exactly ONE cheap-model
 * call, and the other eleven are dropped rather than queued (plan §6.11). It is
 * asserted with a call that is deliberately held open, because a burst against a tier
 * that resolves immediately would pass with no drop rule at all — each round would
 * simply find the slot free again.
 *
 * The second half is the same AC as the other two cheap-tier paths: a failing call
 * must leave the turn — here, the bus fan-out the watcher is subscribed to — completely
 * unaffected, including when the injected tier violates its own type and rejects or
 * throws.
 *
 * Everything runs over a real `Bus` with no server, no database and nothing on the
 * network: the watcher's whole contract is "events in, events out". Assertions come
 * from `node:assert/strict` — jsr.io is unreachable here.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import type { BoughEvent, EventInput, SessionActivityData } from "../schema/events.ts";
import type { Part } from "../schema/parts.ts";
import type { CheapTier } from "../types.ts";
import {
  cheapActivity,
  MAX_BLURB,
  MAX_CODE_CHARS,
  programGist,
  programOf,
  sanitizeBlurb,
  watchActivity,
} from "./activity.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/** The event the turn runner publishes when a `run_steps` call finalizes. */
function runSteps(sessionId: string, code: string): EventInput {
  const part: Part = { type: "tool_call", id: "t1", name: "run_steps", input: { code } };
  return { type: "message.part", sessionId, data: { messageId: "m1", part } };
}

/** `turn.finished` for a session — the watcher's clear trigger. */
function turnFinished(sessionId: string): EventInput {
  return {
    type: "turn.finished",
    sessionId,
    data: { turnId: "t1", sessionId, status: "done" },
  };
}

function activities(events: BoughEvent[]): SessionActivityData[] {
  return events
    .filter((e) => e.type === "session.activity")
    .map((e) => e.data as SessionActivityData);
}

const settle = () => new Promise<void>((r) => setTimeout(r, 0));

interface Rig {
  bus: Bus;
  events: BoughEvent[];
  stop: () => void;
}

function rig(cheap?: CheapTier): Rig {
  const bus = new Bus({ onListenerError: () => {} });
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const stop = watchActivity({ bus, cheap });
  return { bus, events, stop };
}

// ---------------------------------------------------------------------------
// Shaping (pure)
// ---------------------------------------------------------------------------

test("programOf picks out run_steps code and nothing else", () => {
  assert.equal(
    programOf({
      type: "tool_call",
      id: "t",
      name: "run_steps",
      input: { code: "await bash('ls')" },
    }),
    "await bash('ls')",
  );
  // `stop` is the other tool the model sees, and it describes nothing.
  assert.equal(programOf({ type: "tool_call", id: "t", name: "stop", input: {} }), null);
  assert.equal(programOf({ type: "text", text: "hello" }), null);
  assert.equal(programOf({ type: "tool_call", id: "t", name: "run_steps", input: {} }), null);
  assert.equal(
    programOf({ type: "tool_call", id: "t", name: "run_steps", input: { code: "   " } }),
    null,
  );
  assert.equal(programOf(undefined), null);
});

test("programGist truncates from the HEAD — the opening lines are the intent", () => {
  const code = "// INTENT\n" + "x".repeat(MAX_CODE_CHARS + 500) + "\n// FORMATTING";
  const gist = programGist(code);
  assert.ok(gist.includes("// INTENT"));
  assert.ok(!gist.includes("// FORMATTING"));
  assert.ok(gist.endsWith("What is it doing?"));
});

test("sanitizeBlurb takes the first line, unquoted and uncapitalized-period", () => {
  assert.equal(sanitizeBlurb('"running the test suite."'), "running the test suite");
  assert.equal(
    sanitizeBlurb("\n\nrewriting the patch parser\nthen running tests"),
    "rewriting the patch parser",
  );
  assert.equal(sanitizeBlurb("  "), null);
  assert.equal(sanitizeBlurb("y".repeat(MAX_BLURB + 40))?.length, MAX_BLURB);
});

test("cheapActivity is null for empty input without calling anything", async () => {
  const never = { run: () => Promise.reject(new Error("must not be called")) };
  assert.equal(await cheapActivity("   ", { llm: never }), null);
});

// ---------------------------------------------------------------------------
// The watcher
// ---------------------------------------------------------------------------

test("a run_steps round publishes one session.activity blurb", async () => {
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve("running the test suite"),
  });
  try {
    r.bus.publish(runSteps("s1", "await bash('deno test')"));
    await settle();
    assert.deepEqual(activities(r.events), [{
      sessionId: "s1",
      activity: "running the test suite",
    }]);
  } finally {
    r.stop();
  }
});

test("THE DROP RULE: a burst of 12 rounds on one session buys exactly one call", async () => {
  let calls = 0;
  let release: (blurb: string) => void = () => {};
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: () => {
      calls++;
      return new Promise<string>((resolve) => (release = resolve));
    },
  });
  try {
    // The burst lands while the first call is still open. A tier that resolved
    // immediately would find the slot free every time and pass with no rule at all.
    for (let i = 0; i < 12; i++) r.bus.publish(runSteps("s1", `round ${i}`));
    await settle();
    assert.equal(calls, 1, "eleven rounds were DROPPED, not queued");
    assert.equal(activities(r.events).length, 0, "nothing is published until it answers");

    release("running the test suite");
    await settle();
    assert.deepEqual(activities(r.events), [{
      sessionId: "s1",
      activity: "running the test suite",
    }]);

    // …and the slot is released, so the session is describable again. This is the
    // half that makes "drop" survivable: the next round describes itself.
    r.bus.publish(runSteps("s1", "await bash('git commit')"));
    await settle();
    assert.equal(calls, 2);
  } finally {
    r.stop();
  }
});

test("the drop rule is PER SESSION — a burst on one does not silence another", async () => {
  const asked: string[] = [];
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: (recent) => {
      asked.push(recent);
      // The busy session's call never settles, so its slot stays held.
      return recent.includes("busy") ? new Promise<string>(() => {}) : Promise.resolve("listing");
    },
  });
  try {
    for (let i = 0; i < 5; i++) r.bus.publish(runSteps("s-busy", `busy round ${i}`));
    r.bus.publish(runSteps("s-other", "await bash('ls')"));
    await settle();

    // One call for the wedged session (four dropped) and one for the other, which was
    // never blocked by it: the ledger is keyed by session, not global.
    assert.equal(asked.length, 2);
    assert.deepEqual(activities(r.events), [{ sessionId: "s-other", activity: "listing" }]);
  } finally {
    r.stop();
  }
});

test("turn.finished clears the blurb, and a late answer for that turn is discarded", async () => {
  let release: (blurb: string) => void = () => {};
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: () => new Promise<string>((resolve) => (release = resolve)),
  });
  try {
    r.bus.publish(runSteps("s1", "await bash('deno test')"));
    r.bus.publish(turnFinished("s1"));
    await settle();
    assert.deepEqual(activities(r.events), [{ sessionId: "s1", activity: null }]);

    // The answer arrives after the turn ended. It describes nothing current, so it is
    // dropped rather than repainting a status line for finished work.
    release("running the test suite");
    await settle();
    assert.deepEqual(activities(r.events), [{ sessionId: "s1", activity: null }]);
  } finally {
    r.stop();
  }
});

test("a null blurb publishes nothing at all", async () => {
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  });
  try {
    r.bus.publish(runSteps("s1", "await bash('ls')"));
    await settle();
    assert.equal(activities(r.events).length, 0);
  } finally {
    r.stop();
  }
});

// ---------------------------------------------------------------------------
// Failure is a non-event  (the AC)
// ---------------------------------------------------------------------------

test("a REJECTING cheap tier leaves the round's events untouched", async () => {
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.reject(new Error("provider is down")),
  });
  const seen: string[] = [];
  r.bus.subscribe((e) => seen.push(e.type));
  try {
    r.bus.publish(runSteps("s1", "await bash('deno test')"));
    await settle();
    assert.equal(activities(r.events).length, 0, "no blurb, and no error event either");
    // The listener registered after the watcher still received the round (plan §6.6).
    assert.deepEqual(seen, ["message.part"]);
    // …and the slot was released, so the failure does not silence the session forever.
    let answered = false;
    r.stop();
    const r2 = rig({
      title: () => Promise.resolve(null),
      ghostText: () => Promise.resolve(null),
      activity: () => {
        answered = true;
        return Promise.resolve("running the test suite");
      },
    });
    r2.bus.publish(runSteps("s1", "await bash('ls')"));
    await settle();
    assert.ok(answered);
    r2.stop();
  } finally {
    // `r.stop()` already ran inside the body; calling it twice is a no-op.
    r.stop();
  }
});

test("a failure releases the slot on the SAME watcher, not just a fresh one", async () => {
  let calls = 0;
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: () => {
      calls++;
      return calls === 1 ? Promise.reject(new Error("down")) : Promise.resolve("committing");
    },
  });
  try {
    r.bus.publish(runSteps("s1", "round one"));
    await settle();
    r.bus.publish(runSteps("s1", "round two"));
    await settle();
    assert.equal(calls, 2, "the failed call gave its slot back");
    assert.deepEqual(activities(r.events), [{ sessionId: "s1", activity: "committing" }]);
  } finally {
    r.stop();
  }
});

test("a THROWING cheap tier does not break bus fan-out", async () => {
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: () => {
      throw new Error("synchronous explosion");
    },
  });
  const seen: string[] = [];
  r.bus.subscribe((e) => seen.push(e.type));
  try {
    r.bus.publish(runSteps("s1", "await bash('ls')"));
    r.bus.publish(runSteps("s2", "await bash('ls')"));
    await settle();
    assert.deepEqual(seen, ["message.part", "message.part"]);
    assert.equal(activities(r.events).length, 0);
  } finally {
    r.stop();
  }
});

test("no cheap tier at all means no listener work and no events", async () => {
  const r = rig(undefined);
  try {
    r.bus.publish(runSteps("s1", "await bash('ls')"));
    r.bus.publish(turnFinished("s1"));
    await settle();
    assert.equal(activities(r.events).length, 0);
  } finally {
    r.stop();
  }
});

test("unsubscribing stops the watcher", async () => {
  let calls = 0;
  const r = rig({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.resolve(null),
    activity: () => {
      calls++;
      return Promise.resolve("x");
    },
  });
  r.stop();
  r.bus.publish(runSteps("s1", "await bash('ls')"));
  await settle();
  assert.equal(calls, 0);
});
