/**
 * Tests for the cheap tier's shared call and for auto session titles.
 *
 * THE LOAD-BEARING ONE is the group under "failure is a non-event": a cheap-model call
 * that throws, rejects, returns junk or never answers must leave the turn *byte for
 * byte* what it would have been. That is asserted through the real
 * `POST /sessions/:id/messages` handler rather than against `maybeAutoTitle` alone,
 * because the guarantee plan §8.4 asks for is about the MESSAGE PATH — a unit test of
 * the titler would keep passing on the day someone awaited it inside the handler.
 *
 * The second group pins the deadline. It is the failure a try/catch does not cover: a
 * provider that neither answers nor errors would otherwise leave the promise pending
 * forever, and for the sibling activity watcher that means a session's one blurb slot
 * is held for the life of the process.
 *
 * Everything is offline: no test constructs a real client, and `cheapText` is always
 * given an injected `LlmClient`. Assertions come from `node:assert/strict` — jsr.io is
 * unreachable here and a test that cannot run offline does not belong in
 * `deno task test`.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { BoughEvent } from "../schema/events.ts";
import type { Message, Session } from "../schema/parts.ts";
import { createHandler, type Route, route } from "../server/app.ts";
import {
  createSession,
  postMessage,
  type TurnStarter,
  type WithTurnStarter,
} from "../server/sessions.ts";
import type { AppCtx, CheapTier, LlmClient, LlmResult } from "../types.ts";
import {
  CHEAP_MODEL_ENV,
  cheapModel,
  cheapText,
  cheapTitle,
  DEFAULT_CHEAP_MODEL,
  maybeAutoTitle,
  sanitizeTitle,
  userText,
  watchTitles,
} from "./titles.ts";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/** An `LlmClient` that answers with one text block. */
function sayingClient(text: string): LlmClient {
  return {
    run: () =>
      Promise.resolve<LlmResult>({
        content: [{ type: "text", text }],
        stopReason: "end_turn",
      }),
  };
}

/** An `LlmClient` that never settles until its signal aborts. */
function hangingClient(): LlmClient {
  return {
    run: (_params, _onText, signal) =>
      new Promise<LlmResult>((_resolve, reject) => {
        signal?.addEventListener("abort", () => reject(new Error("aborted")));
      }),
  };
}

const TABLE: Route[] = [
  route("POST", "/sessions", createSession),
  route("POST", "/sessions/:id/messages", postMessage),
];

interface Fixture {
  call: (req: Request) => Promise<Response>;
  ctx: AppCtx & WithTurnStarter;
  db: SqliteDb;
  events: BoughEvent[];
  started: { session: Session; message: Message }[];
  stop: () => void;
}

function fixture(cheap?: CheapTier): Fixture {
  const db = openDb(":memory:");
  const bus = new Bus({ onListenerError: () => {} });
  const events: BoughEvent[] = [];
  bus.subscribe((e) => events.push(e));
  const started: { session: Session; message: Message }[] = [];
  const startTurn: TurnStarter = (_c, session, message) => started.push({ session, message });
  const ctx: AppCtx & WithTurnStarter = { db, bus, model: "test-model", startTurn, cheap };
  const stop = watchTitles(ctx);
  return { call: createHandler(ctx, { routes: TABLE }), ctx, db, events, started, stop };
}

const url = (path: string) => `http://127.0.0.1:4321${path}`;

async function newSession(f: Fixture, title?: string): Promise<Session> {
  const res = await f.call(
    new Request(url("/sessions"), {
      method: "POST",
      body: JSON.stringify(title === undefined ? {} : { title }),
      headers: { "content-type": "application/json" },
    }),
  );
  return (await res.json()) as Session;
}

async function post(f: Fixture, id: string, text: string): Promise<Response> {
  return await f.call(
    new Request(url(`/sessions/${id}/messages`), {
      method: "POST",
      body: JSON.stringify({ text }),
      headers: { "content-type": "application/json" },
    }),
  );
}

/** Let every already-queued microtask and the promise chains behind them run. */
const settle = () => new Promise<void>((r) => setTimeout(r, 0));

// ---------------------------------------------------------------------------
// The shared call
// ---------------------------------------------------------------------------

test("cheapText returns the concatenated text of a successful round", async () => {
  const text = await cheapText({
    system: "s",
    prompt: "p",
    maxTokens: 16,
    llm: sayingClient("  hello  "),
  });
  assert.equal(text, "hello");
});

test("cheapText resolves null for every provider failure — it never rejects", async () => {
  const throwsSync: LlmClient = {
    run: () => {
      throw new Error("no API key for anthropic");
    },
  };
  const rejects: LlmClient = { run: () => Promise.reject(new Error("500 overloaded")) };
  const empty: LlmClient = {
    run: () => Promise.resolve<LlmResult>({ content: [], stopReason: "end_turn" }),
  };
  const blank: LlmClient = {
    run: () =>
      Promise.resolve<LlmResult>({
        content: [{ type: "text", text: "   " }],
        stopReason: "end_turn",
      }),
  };
  for (const llm of [throwsSync, rejects, empty, blank]) {
    assert.equal(await cheapText({ system: "s", prompt: "p", maxTokens: 16, llm }), null);
  }
});

test("cheapText abandons a hung provider at its deadline", async () => {
  // The failure a try/catch does not cover. Without this the promise never settles,
  // and the activity watcher's one-slot-per-session ledger would never be released.
  const started = Date.now();
  const text = await cheapText({
    system: "s",
    prompt: "p",
    maxTokens: 16,
    llm: hangingClient(),
    timeoutMs: 20,
  });
  assert.equal(text, null);
  assert.ok(Date.now() - started < 5_000, "the deadline, not the test runner, ended it");
});

test("the cheap model is read per call, and defaults when unset", () => {
  assert.equal(cheapModel(() => undefined), DEFAULT_CHEAP_MODEL);
  assert.equal(cheapModel(() => "   "), DEFAULT_CHEAP_MODEL);
  assert.equal(
    cheapModel((k) => (k === CHEAP_MODEL_ENV ? "openai:gpt-5-mini" : undefined)),
    "openai:gpt-5-mini",
  );
});

// ---------------------------------------------------------------------------
// Sanitizing
// ---------------------------------------------------------------------------

/**
 * Asked to "List the numbers 1 to 60", the cheap tier titled a session `1` — a row in the
 * switcher that names nothing, on the one surface where every row has to be recognisable.
 * Refusing is better than a bad name: the session falls back to its workspace, which is
 * at least true.
 */
/**
 * The TUI names a conversation after the shell command that created it, so a `!`-only conversation
 * is not "(untitled)" forever. This guard only ever replaced an EMPTY title, so one that went on
 * to do real work stayed called `! ls -1 src` — seen on a fresh-install walk where the first thing
 * typed was `!ls -1 src` and the second was a real task.
 */
test("a `! command` title is provisional and gets replaced by a real one", async () => {
  const f = fixture({ title: () => Promise.resolve("Add a discount helper"), ghost: () => Promise.resolve(null), activity: () => Promise.resolve(null) } as unknown as CheapTier);
  try {
    // The TUI writes this when `!command` creates the conversation (`store.runShell`).
    const provisional = await newSession(f, "! ls -1 src");
    await post(f, provisional.id, "Add a discount(items, pct) helper to src/cart.py");
    await settle();
    await settle();
    assert.equal(f.db.getSession(provisional.id)?.title, "Add a discount helper");

    // A title the user or the cheap tier already chose is left alone: the provisional rule is only
    // for the `! ` prefix the shell path writes.
    const chosen = await newSession(f, "Pricing rewrite");
    await post(f, chosen.id, "and now the shipping rules");
    await settle();
    await settle();
    assert.equal(f.db.getSession(chosen.id)?.title, "Pricing rewrite");
  } finally {
    f.stop();
    f.db.close();
  }
});

test("sanitizeTitle refuses a title that carries no information", () => {
  assert.equal(sanitizeTitle("1"), "");
  assert.equal(sanitizeTitle("42"), "");
  assert.equal(sanitizeTitle("-"), "");
  assert.equal(sanitizeTitle("ok"), "");
  assert.equal(sanitizeTitle("1 2 3"), "");
  // Three letters is the floor, and real short titles clear it.
  assert.equal(sanitizeTitle("Bug"), "Bug");
  assert.equal(sanitizeTitle("CI fix"), "CI fix");
  assert.equal(sanitizeTitle("Fix cart pricing"), "Fix cart pricing");
});

test("sanitizeTitle strips the label, the quoting and the trailing period", () => {
  assert.equal(sanitizeTitle('Title: "Fix the patch parser."'), "Fix the patch parser");
  assert.equal(sanitizeTitle("\n\n  rewrite the theme route  \n"), "rewrite the theme route");
  assert.equal(sanitizeTitle("**bold answer**"), "bold answer");
});

test("sanitizeTitle caps a model that answered the message instead of titling it", () => {
  // The live finding the word cap exists for: a session titled with thirteen words
  // of story. It is now REFUSED outright rather than capped — eight words of an
  // answer is still an answer, and a session with no title falls back to its
  // workspace name, which is at least true.
  const prose = "Sure, I can help you with that — let me start by reading the file";
  assert.equal(sanitizeTitle(prose), "");
  // Prose that is not a reply is still capped to a readable stub.
  assert.equal(
    sanitizeTitle("Rewrite the theme route and then repaint every preview row").split(/\s+/).length,
    8,
  );
  assert.equal(sanitizeTitle("theme picker previews live"), "theme picker previews live");
});

test("cheapTitle is null for empty input and for an unusable answer", async () => {
  assert.equal(await cheapTitle("   ", { llm: sayingClient("x") }), null);
  assert.equal(await cheapTitle("hello", { llm: sayingClient('""') }), null);
  assert.equal(await cheapTitle("hello", { llm: sayingClient("Fix it") }), "Fix it");
});

test("userText joins the text parts and ignores everything else", () => {
  const message: Message = {
    id: "m",
    sessionId: "s",
    role: "user",
    pending: false,
    createdAt: 0,
    parts: [
      { type: "text", text: "look at" },
      { type: "image", path: "/x.png", mediaType: "image/png", name: "x.png", size: 1 },
      { type: "text", text: "this" },
    ],
  };
  assert.equal(userText(message), "look at\nthis");
});

// ---------------------------------------------------------------------------
// Auto-titling
// ---------------------------------------------------------------------------

test("a posted first message names the untitled session and announces it", async () => {
  const f = fixture({
    title: () => Promise.resolve("fix the patch parser"),
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  });
  try {
    const session = await newSession(f);
    assert.equal(session.title, "");
    await post(f, session.id, "the patch parser drops the last line");
    await settle();

    assert.equal(f.db.getSession(session.id)?.title, "fix the patch parser");
    const updated = f.events.filter((e) => e.type === "session.updated");
    assert.equal(updated.length, 1, "one session.updated re-renders every sidebar");
    assert.equal((updated[0].data as Session).title, "fix the patch parser");
  } finally {
    f.stop();
    f.db.close();
  }
});

test("a session that already has a title is never re-titled and never billed", async () => {
  let calls = 0;
  const f = fixture({
    title: () => {
      calls++;
      return Promise.resolve("generated");
    },
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  });
  try {
    const session = await newSession(f, "the name I chose");
    await post(f, session.id, "hello");
    await settle();
    assert.equal(calls, 0);
    assert.equal(f.db.getSession(session.id)?.title, "the name I chose");
  } finally {
    f.stop();
    f.db.close();
  }
});

test("a rename during the round-trip is not clobbered by the answer", async () => {
  let release: (title: string) => void = () => {};
  const f = fixture({
    title: () => new Promise<string>((r) => (release = r)),
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  });
  try {
    const session = await newSession(f);
    await post(f, session.id, "hello");
    // The user renames while the cheap model is still thinking.
    f.db.setSessionTitle(session.id, "mine");
    release("the model's idea");
    await settle();
    assert.equal(f.db.getSession(session.id)?.title, "mine");
  } finally {
    f.stop();
    f.db.close();
  }
});

test("two messages in quick succession buy exactly one title", async () => {
  let calls = 0;
  let release: (title: string) => void = () => {};
  const f = fixture({
    title: () => {
      calls++;
      return new Promise<string>((r) => (release = r));
    },
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  });
  try {
    const session = await newSession(f);
    await post(f, session.id, "first");
    await post(f, session.id, "second");
    assert.equal(calls, 1, "the second post rides the in-flight title, it does not buy one");
    release("one title");
    await settle();
    assert.equal(f.db.getSession(session.id)?.title, "one title");
  } finally {
    f.stop();
    f.db.close();
  }
});

test("an images-only message buys no title", async () => {
  let calls = 0;
  const f = fixture({
    title: () => {
      calls++;
      return Promise.resolve("nope");
    },
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  });
  try {
    const session = await newSession(f);
    maybeAutoTitle(f.ctx, session.id, "   ");
    await settle();
    assert.equal(calls, 0);
  } finally {
    f.stop();
    f.db.close();
  }
});

// ---------------------------------------------------------------------------
// Failure is a non-event  (the AC)
// ---------------------------------------------------------------------------

test("a REJECTING cheap tier leaves the message path completely unaffected", async () => {
  const f = fixture({
    // A tier that violates its own contract, which is the worst case: the type says
    // these never reject, but an implementation is not bound by a type.
    title: () => Promise.reject(new Error("provider is down")),
    ghostText: () => Promise.reject(new Error("provider is down")),
    activity: () => Promise.reject(new Error("provider is down")),
  });
  try {
    const session = await newSession(f);
    const res = await post(f, session.id, "the patch parser drops the last line");
    await settle();

    // The turn is untouched: accepted, persisted, announced, and handed to the runner.
    assert.equal(res.status, 202);
    assert.equal(f.started.length, 1, "the turn started");
    assert.equal(f.db.messagesFor(session.id).length, 1);
    assert.equal(
      f.events.filter((e) => e.type === "message.started").length,
      1,
      "the user message was announced exactly once",
    );
    // The only consequence is the one the spec allows: the session keeps its
    // placeholder. Annoying, not broken.
    assert.equal(f.db.getSession(session.id)?.title, "");
    assert.equal(f.events.filter((e) => e.type === "session.updated").length, 0);
  } finally {
    f.stop();
    f.db.close();
  }
});

test("a THROWING cheap tier does not break bus fan-out to other subscribers", async () => {
  const f = fixture({
    title: () => {
      throw new Error("synchronous explosion");
    },
    ghostText: () => Promise.resolve(null),
    activity: () => Promise.resolve(null),
  });
  const seen: string[] = [];
  f.ctx.bus.subscribe((e) => seen.push(e.type));
  try {
    const session = await newSession(f);
    const res = await post(f, session.id, "hello");
    await settle();
    assert.equal(res.status, 202);
    // The listener registered AFTER the titler still received the event (plan §6.6).
    assert.ok(seen.includes("message.started"));
    assert.equal(f.db.getSession(session.id)?.title, "");
  } finally {
    f.stop();
    f.db.close();
  }
});

test("no cheap tier at all is a working server, not a degraded one", async () => {
  const f = fixture(undefined);
  try {
    const session = await newSession(f);
    const res = await post(f, session.id, "hello");
    await settle();
    assert.equal(res.status, 202);
    assert.equal(f.started.length, 1);
    assert.equal(f.db.getSession(session.id)?.title, "");
  } finally {
    f.stop();
    f.db.close();
  }
});

test("a model that answered instead of titling yields NO title, not a truncated lie", () => {
  // The live header, from a session where bough had just read the file it claimed
  // no access to. Eight words of an answer is not a title, it is a false sentence
  // shown above every screen.
  assert.equal(sanitizeTitle("I don't have access to your codebase, so I can't say"), "");
  for (
    const reply of [
      "I'll take a look at that for you",
      "Sure! Here is what that file does",
      "Sorry, I cannot help with that request",
      "Let me explain what the runner module does",
      "As an AI assistant I should note that",
      "Based on the code you have shared with me",
      "You asked about the turn runner and its",
    ]
  ) {
    assert.equal(sanitizeTitle(reply), "", `should be refused: ${reply}`);
  }
  // A cap that would leave a dangling connective is refused too — it reads as a
  // half-finished thought rather than a name.
  assert.equal(
    sanitizeTitle("Refactor the parser and the lexer and the rest"),
    "Refactor the parser and the lexer",
  );
});

test("a real title still passes through untouched", () => {
  assert.equal(
    sanitizeTitle("Fix division by zero in calculator"),
    "Fix division by zero in calculator",
  );
  assert.equal(sanitizeTitle("Title: Add retry to the LLM client"), "Add retry to the LLM client");
  assert.equal(sanitizeTitle('"Wire up the changes rail"'), "Wire up the changes rail");
  // "Interrupt" starts with I but is not the pronoun — the boundary must hold.
  assert.equal(
    sanitizeTitle("Interrupt handling for running turns"),
    "Interrupt handling for running turns",
  );
  assert.equal(sanitizeTitle("Image input support"), "Image input support");
});
