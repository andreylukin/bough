/**
 * The freeze test. T-1 declares contracts every other task codes against, so what
 * is worth asserting here is not that Zod works — it is that the *decisions* in
 * the contract are still the ones the spec made:
 *
 *   - the six part kinds, and nothing else,
 *   - a Session with no archive/deprecate field (visibility is derived),
 *   - the closed event-name set,
 *   - one canonical host-name list, with the removed verbs actually gone.
 *
 * Each of these is a thing a later task could quietly re-add. This test is what
 * makes that show up as a red run rather than as a second source of truth.
 */
import { test } from "bun:test";
import { Message, Part, Session, Turn } from "./parts.ts";
import { BoughEvent, EVENT_TYPES } from "./events.ts";
import { CompactBody, CreateSessionBody, PartPick } from "./requests.ts";
import { HOST_FN_NAMES, PROGRAM_PARAMS } from "../harness/protocol.ts";

// Local assertions rather than `@std/assert`, so this file has zero registry
// dependencies: T-1 is the task that unblocks everyone else, and its own test
// must run before anything has been fetched.
function assert(cond: unknown, msg = "assertion failed"): asserts cond {
  if (!cond) throw new Error(msg);
}
function assertEquals(actual: unknown, expected: unknown, msg?: string): void {
  const a = JSON.stringify(actual), b = JSON.stringify(expected);
  if (a !== b) throw new Error(msg ?? `expected ${b}, got ${a}`);
}
function assertThrows(fn: () => unknown, msg = "expected a throw"): void {
  try {
    fn();
  } catch {
    return;
  }
  throw new Error(msg);
}

test("every part kind round-trips", () => {
  const parts = [
    { type: "text", text: "hi" },
    { type: "reasoning", text: "thinking" },
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: "1" } },
    { type: "tool_result", callId: "c1", output: "out", isError: false, interrupted: true },
    { type: "image", path: "/a/b.png", mediaType: "image/png", name: "b.png", size: 12 },
    {
      type: "ask",
      id: "q1",
      question: "which?",
      options: ["a", "b"],
      status: "answered",
      answer: "a",
    },
  ];
  for (const raw of parts) {
    assertEquals(Part.parse(raw), raw);
  }
  assertEquals(parts.length, 6, "spec §4 names exactly six part kinds");
});

test("the part union is closed — removed kinds are rejected", () => {
  // `prose` existed in the old tree; `run_steps` output is not a part kind.
  assertThrows(() => Part.parse({ type: "prose", text: "x" }));
  assertThrows(() => Part.parse({ type: "worker", text: "x" }));
});

test("a message carries an ordered parts array and a pending flag", () => {
  const m = Message.parse({
    id: "m1",
    sessionId: "s1",
    role: "supervisor",
    parts: [{ type: "text", text: "hi" }],
    pending: true,
    createdAt: 1,
  });
  assertEquals(m.parts.length, 1);
  assert(m.pending);
  // `worker` is not a role any more — user | supervisor | system.
  assertThrows(() => Message.parse({ ...m, role: "worker" }));
});

test("visibility is derived: a Session has no archive/deprecate field", () => {
  const s = Session.parse({
    id: "s1",
    title: "t",
    kind: "subagent",
    createdAt: 1,
    parentId: null,
    originId: "s0",
    // Anything the old contract stored to hide a session must not survive parsing.
    archivedAt: 123,
    deprecatedAt: 456,
  });
  assert(!("archivedAt" in s), "archivedAt is not part of the contract");
  assert(!("deprecatedAt" in s), "deprecatedAt is not part of the contract");
  assertEquals(s.kind, "subagent");
  assertEquals(s.originId, "s0");
});

test("session kinds are the five of spec §4", () => {
  for (const kind of ["root", "fork", "compaction", "subagent", "workflow_agent"]) {
    assertEquals(
      Session.parse({ id: "s", title: "t", kind, createdAt: 0, parentId: null }).kind,
      kind,
    );
  }
  assertThrows(() =>
    Session.parse({ id: "s", title: "t", kind: "worker", createdAt: 0, parentId: null })
  );
});

test("a turn can be orphaned — restart recovery depends on it", () => {
  const t = Turn.parse({
    id: "t1",
    sessionId: "s1",
    messageId: "m1",
    status: "orphaned",
    step: "round 2",
    createdAt: 1,
    updatedAt: 2,
  });
  assertEquals(t.status, "orphaned");
  for (const status of ["running", "done", "error", "interrupted", "orphaned"]) {
    assertEquals(Turn.parse({ ...t, status }).status, status);
  }
});

test("the event-name set is closed and stamped", () => {
  // Spec §3's list, plus tool.log (spec §5.3's live console stream).
  assertEquals(EVENT_TYPES.length, 16);
  assert(EVENT_TYPES.includes("turn.finished"));
  assert(EVENT_TYPES.includes("tool.log"));
  const e = BoughEvent.parse({
    type: "message.delta",
    sessionId: "s1",
    seq: 7,
    ts: 1,
    data: { messageId: "m1", delta: "x" },
  });
  assertEquals(e.seq, 7);
  assertThrows(() => BoughEvent.parse({ type: "message.invented", seq: 1, ts: 1 }));
});

test("host names: one list, with the dropped verbs actually dropped", () => {
  assertEquals(new Set(HOST_FN_NAMES).size, HOST_FN_NAMES.length, "no duplicate host names");
  // Spec §17: one editing idiom, no output digestion, no semantic recall. `fetch`
  // and `mcp` were bridged once and are not any more: HTTP is the runtime's own
  // fetch, and an MCP tool is called through `bough mcp call` in the shell — so
  // neither may be re-added here without the prompt section to match.
  for (const gone of ["read", "edit", "extract", "recall", "fetch", "mcp", "mcpStatus", "image"]) {
    assert(!(HOST_FN_NAMES as readonly string[]).includes(gone), `${gone} must not be bridged`);
  }
  // Declared now even though M6 implements them — the list is frozen.
  for (
    const later of [
      "ask",
      "state",
      "schedule",
      "artifact",
      "workflow",
    ]
  ) {
    assert((HOST_FN_NAMES as readonly string[]).includes(later), `${later} must be declared`);
  }
  // PROGRAM_PARAMS is the host names first, then the two names bound the same way
  // that are not bridged calls: `console` (built worker-side) and `require` (a real
  // CommonJS require, so a program can reach `node:*` without an import statement).
  assertEquals(PROGRAM_PARAMS.slice(0, HOST_FN_NAMES.length), HOST_FN_NAMES);
  assertEquals(PROGRAM_PARAMS.slice(HOST_FN_NAMES.length), ["console", "require"]);
});

test("request bodies reject the empty selections that make a no-op branch", () => {
  assertThrows(() => PartPick.parse({ messageId: "m1", parts: [] }));
  assertEquals(PartPick.parse({ messageId: "m1" }).parts, undefined);
  assertThrows(() => CompactBody.parse({ picks: [] }));
  // Everything on a session create is optional — an untitled root is legal.
  assertEquals(CreateSessionBody.parse({}), {});
});
