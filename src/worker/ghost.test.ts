/**
 * Tests for composer ghost text.
 *
 * THE LOAD-BEARING ONE: every cheap-model failure is `200 {ghost: null}`. A 5xx here
 * would put an error banner on a composer for a feature whose entire value is that the
 * user can ignore it — and it is the one path of the three where a failure can reach a
 * client directly, since titles and blurbs are bus listeners nobody is awaiting.
 *
 * The pure shaping is tested directly, and the tail-keeping deliberately so: `renderConvo`
 * truncates from the FRONT, which is backwards from every other truncation in the tree.
 * An agent's reply ends with the outcome and what it proposes next, and that ending is
 * the entire signal for predicting the follow-up — keeping the head would feed the model
 * the preamble and drop the conclusion.
 *
 * Everything runs through `createHandler(ctx)` over an in-memory database with no socket
 * bound and nothing on the network. Assertions come from `node:assert/strict`: jsr.io is
 * unreachable here.
 */
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { Message, Session } from "../schema/parts.ts";
import { createHandler, type Route, route } from "../server/app.ts";
import type { AppCtx, CheapTier } from "../types.ts";
import {
  cheapGhost,
  convoFrom,
  type ConvoLine,
  ghostFor,
  ghostPrompt,
  ghostTextH,
  MAX_LINE_CHARS,
  MAX_LINES,
  MAX_SUGGESTION,
  renderConvo,
  sanitizeSuggestion,
} from "./ghost.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

const TABLE: Route[] = [route("POST", "/sessions/:id/ghost", ghostTextH)];

interface Fixture {
  call: (path: string, body?: unknown) => Promise<{ status: number; body: unknown }>;
  ctx: AppCtx;
  db: SqliteDb;
  sessionId: string;
}

function fixture(cheap?: CheapTier): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus();
  const ctx: AppCtx = { db, bus, cheap };
  const session: Session = {
    id: "s1",
    title: "",
    kind: "root",
    createdAt: Date.now(),
    parentId: null,
  };
  db.createSession(session);
  const handler = createHandler(ctx, { routes: TABLE });
  return {
    ctx,
    db,
    sessionId: session.id,
    call: async (path, body) => {
      const res = await handler(
        new Request(`http://127.0.0.1:4321${path}`, {
          method: "POST",
          ...(body === undefined ? {} : {
            body: JSON.stringify(body),
            headers: { "content-type": "application/json" },
          }),
        }),
      );
      return { status: res.status, body: await res.json() };
    },
  };
}

let seq = 0;
function say(db: SqliteDb, sessionId: string, role: Message["role"], text: string): void {
  db.createMessage({
    id: `m${++seq}`,
    sessionId,
    role,
    parts: [{ type: "text", text }],
    pending: false,
    createdAt: Date.now() + seq,
  });
}

const tier = (ghost: string | null | Promise<string | null>): CheapTier => ({
  title: () => Promise.resolve(null),
  ghostText: () => Promise.resolve(ghost),
  activity: () => Promise.resolve(null),
});

// ---------------------------------------------------------------------------
// Shaping (pure)
// ---------------------------------------------------------------------------

Deno.test("renderConvo keeps the TAIL of a long line, not the head", () => {
  const long = "PREAMBLE".padEnd(MAX_LINE_CHARS + 200, "x") + "THE-CONCLUSION";
  const rendered = renderConvo([{ role: "agent", text: long }]);
  assert.ok(rendered.endsWith("THE-CONCLUSION"), "the outcome survives");
  assert.ok(!rendered.includes("PREAMBLE"), "the preamble is what gets dropped");
  assert.ok(rendered.startsWith("agent: …"));
});

Deno.test("renderConvo keeps only the last MAX_LINES turns, oldest first", () => {
  const lines: ConvoLine[] = Array.from({ length: MAX_LINES + 4 }, (_, i) => ({
    role: i % 2 === 0 ? "user" : "agent",
    text: `line${i}`,
  }));
  const rendered = renderConvo(lines).split("\n");
  assert.equal(rendered.length, MAX_LINES);
  assert.ok(rendered[0].endsWith(`line${lines.length - MAX_LINES}`));
  assert.ok(rendered.at(-1)?.endsWith(`line${lines.length - 1}`));
});

Deno.test("a typed prefix becomes a continuation instruction", () => {
  const lines: ConvoLine[] = [{ role: "agent", text: "done" }];
  assert.match(ghostPrompt(lines), /The user's next message:/);
  const withPrefix = ghostPrompt(lines, "  run the  ");
  assert.match(withPrefix, /has started typing: run the/);
  assert.match(withPrefix, /starting from what they typed/);
});

Deno.test("convoFrom reduces a thread and treats system notes as user-side text", () => {
  const messages: Message[] = [
    {
      id: "1",
      sessionId: "s",
      role: "user",
      pending: false,
      createdAt: 1,
      parts: [{ type: "text", text: "go" }],
    },
    {
      id: "2",
      sessionId: "s",
      role: "supervisor",
      pending: false,
      createdAt: 2,
      parts: [
        { type: "reasoning", text: "thinking" },
        { type: "text", text: "done" },
      ],
    },
    {
      id: "3",
      sessionId: "s",
      role: "system",
      pending: false,
      createdAt: 3,
      parts: [{ type: "text", text: "[background] bg_1 finished" }],
    },
    { id: "4", sessionId: "s", role: "supervisor", pending: false, createdAt: 4, parts: [] },
  ];
  assert.deepEqual(convoFrom(messages), [
    { role: "user", text: "go" },
    // Reasoning is display-only and never reaches a prompt (plan §6.4).
    { role: "agent", text: "done" },
    { role: "user", text: "[background] bg_1 finished" },
  ]);
});

Deno.test("sanitizeSuggestion unlabels, unquotes and caps", () => {
  assert.equal(sanitizeSuggestion('next: "run the tests"'), "run the tests");
  assert.equal(sanitizeSuggestion("\n\ncommit it\nand push"), "commit it");
  assert.equal(sanitizeSuggestion("   "), null);
  assert.equal(sanitizeSuggestion("x".repeat(MAX_SUGGESTION + 50))?.length, MAX_SUGGESTION);
});

Deno.test("cheapGhost is null for an empty prompt without calling anything", async () => {
  const never = { run: () => Promise.reject(new Error("must not be called")) };
  assert.equal(await cheapGhost("  ", { llm: never }), null);
});

// ---------------------------------------------------------------------------
// The feature
// ---------------------------------------------------------------------------

Deno.test("a session with history gets a suggestion", async () => {
  const f = fixture(tier("run the tests"));
  try {
    say(f.db, f.sessionId, "user", "add the theme route");
    say(f.db, f.sessionId, "supervisor", "added it; the tests are not run yet");
    const res = await f.call(`/sessions/${f.sessionId}/ghost`);
    assert.equal(res.status, 200);
    assert.deepEqual(res.body, { ghost: "run the tests" });
  } finally {
    f.db.close();
  }
});

Deno.test("an empty conversation is a null ghost, and buys nothing", async () => {
  let calls = 0;
  const f = fixture({
    title: () => Promise.resolve(null),
    ghostText: () => {
      calls++;
      return Promise.resolve("nope");
    },
    activity: () => Promise.resolve(null),
  });
  try {
    const res = await f.call(`/sessions/${f.sessionId}/ghost`);
    assert.deepEqual(res.body, { ghost: null });
    assert.equal(calls, 0, "there is nothing to predict from");
  } finally {
    f.db.close();
  }
});

Deno.test("the typed prefix reaches the model", async () => {
  let seen = "";
  const f = fixture({
    title: () => Promise.resolve(null),
    ghostText: (prefix) => {
      seen = prefix;
      return Promise.resolve("run the tests");
    },
    activity: () => Promise.resolve(null),
  });
  try {
    say(f.db, f.sessionId, "user", "add the theme route");
    await f.call(`/sessions/${f.sessionId}/ghost`, { prefix: "run the" });
    assert.match(seen, /has started typing: run the/);
  } finally {
    f.db.close();
  }
});

Deno.test("an unknown session is the only failure that is not a 200", async () => {
  const f = fixture(tier("x"));
  try {
    const res = await f.call("/sessions/nope/ghost");
    assert.equal(res.status, 404);
    assert.match((res.body as { error: string }).error, /nope/);
  } finally {
    f.db.close();
  }
});

// ---------------------------------------------------------------------------
// Failure is a non-event  (the AC)
// ---------------------------------------------------------------------------

Deno.test("a REJECTING cheap tier is 200 {ghost: null}, never a 5xx", async () => {
  const f = fixture({
    title: () => Promise.resolve(null),
    ghostText: () => Promise.reject(new Error("provider is down")),
    activity: () => Promise.resolve(null),
  });
  try {
    say(f.db, f.sessionId, "user", "add the theme route");
    const res = await f.call(`/sessions/${f.sessionId}/ghost`);
    assert.equal(res.status, 200);
    assert.deepEqual(res.body, { ghost: null });
  } finally {
    f.db.close();
  }
});

Deno.test("a THROWING cheap tier is 200 {ghost: null} too", async () => {
  const f = fixture({
    title: () => Promise.resolve(null),
    ghostText: () => {
      throw new Error("synchronous explosion");
    },
    activity: () => Promise.resolve(null),
  });
  try {
    say(f.db, f.sessionId, "user", "hello");
    const res = await f.call(`/sessions/${f.sessionId}/ghost`);
    assert.equal(res.status, 200);
    assert.deepEqual(res.body, { ghost: null });
  } finally {
    f.db.close();
  }
});

Deno.test("no cheap tier at all answers null rather than failing", async () => {
  const f = fixture(undefined);
  try {
    say(f.db, f.sessionId, "user", "hello");
    assert.deepEqual((await f.call(`/sessions/${f.sessionId}/ghost`)).body, { ghost: null });
    assert.equal(await ghostFor(f.ctx, f.sessionId), null);
  } finally {
    f.db.close();
  }
});
