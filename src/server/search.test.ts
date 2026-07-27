/**
 * Tests for keyword search over transcripts.
 *
 * The load-bearing one is the last group: **an FTS write failure must not break message
 * insertion.** It is asserted against a genuinely broken index — the `messages_fts`
 * table is dropped out from under a live handle through a second connection, so
 * `indexMessage` fails the way it would in production rather than the way a stub says
 * it would — and it is asserted through the real `POST /sessions/:id/messages` handler,
 * because the guarantee is about the write path and a unit test of the wrapper alone
 * would prove nothing about who calls it.
 *
 * The query tests pin FTS5 semantics the module documents and the UI depends on: bare
 * words are an implicit AND, a quoted phrase requires adjacency, and ranking puts the
 * denser match first. These are properties of SQLite, not of this module — which is
 * exactly why they are pinned here: the module tells users bare words are ANDed, and if
 * that ever stopped being true the promise in the error text would be a lie.
 *
 * Everything runs against `createHandler(ctx)` over a database with no socket bound and
 * nothing on the network (plan §7). Two tests need a real FILE — `:memory:` cannot be
 * opened twice — and use `Deno.makeTempDir`, never `~/.bough`.
 *
 * Assertions come from `node:assert/strict` rather than `@std/assert`: jsr.io is not
 * reachable from this environment, and a test that cannot run offline does not belong
 * in `deno task test`.
 */
import assert from "node:assert/strict";
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { Bus } from "../bus.ts";
import { openDb, type SqliteDb } from "../db/db.ts";
import type { Message, Part, Session, SessionKind } from "../schema/parts.ts";
import type { AppCtx, Db } from "../types.ts";
import { createHandler, type Route, route } from "./app.ts";
import { postMessage, type TurnStarter, type WithTurnStarter } from "./sessions.ts";
import {
  DEFAULT_LIMIT,
  indexHealth,
  indexRecoveredMessages,
  quoteQuery,
  rebuildIndex,
  reindexH,
  searchH,
  type SearchResult,
  searchSafeDb,
  searchTranscripts,
} from "./search.ts";

// ---- fixtures ---------------------------------------------------------------

/** This task's two entries, plus the message intake the failure test drives. */
const TABLE: Route[] = [
  route("GET", "/search", searchH),
  route("POST", "/search/reindex", reindexH),
  route("POST", "/sessions/:id/messages", postMessage),
];

interface Fixture {
  call: (req: Request) => Promise<Response>;
  ctx: AppCtx & WithTurnStarter;
  db: Db;
  raw: SqliteDb;
  close: () => void;
}

/**
 * A fabricated ctx whose `db` is wrapped exactly as `main.ts` wraps it, so every test
 * here exercises the handle the server actually serves with. `path` opts into a real
 * file for the tests that need a second connection.
 */
function fixture(opts: { path?: string; onIndexError?: () => void } = {}): Fixture {
  const raw = openDb(opts.path ?? ":memory:");
  const db = searchSafeDb(raw, { onError: opts.onIndexError ?? (() => {}) });
  const starter: TurnStarter = () => {};
  const ctx: AppCtx & WithTurnStarter = { db, bus: new Bus(), startTurn: starter };
  return {
    call: createHandler(ctx, { routes: TABLE, onUnexpectedError: () => {} }),
    ctx,
    db,
    raw,
    close: () => raw.close(),
  };
}

let clock = 1_700_000_000_000;

/** A session row, written straight through the port — no HTTP needed to search. */
function session(db: Db, title: string, kind: SessionKind = "root", originId?: string): Session {
  return db.createSession({
    id: crypto.randomUUID(),
    title,
    kind,
    createdAt: clock++,
    parentId: null,
    ...(originId ? { originId } : {}),
  });
}

/** A message plus its index write, exactly as every insert path does it. */
function say(db: Db, s: Session, text: string, role: Message["role"] = "user"): Message {
  const parts: Part[] = [{ type: "text", text }];
  const stored = db.createMessage({
    id: crypto.randomUUID(),
    sessionId: s.id,
    role,
    parts,
    pending: false,
    createdAt: clock++,
  });
  db.indexMessage(stored);
  return stored;
}

const url = (path: string) => `http://127.0.0.1:4321${path}`;
const search = async (f: Fixture, query: string): Promise<SearchResult> => {
  const res = await f.call(new Request(url(`/search?${query}`)));
  assert.equal(res.status, 200, await res.clone().text());
  return await res.json() as SearchResult;
};

// ---- querying ---------------------------------------------------------------

Deno.test("a multi-word query is an implicit AND, not a phrase and not an OR", async () => {
  const f = fixture();
  try {
    const s = session(f.db, "index work");
    const both = say(f.db, s, "the patch engine rejects an overlapping conflict range");
    const adjacent = say(f.db, s, "a patch conflict names the file");
    say(f.db, s, "patch applies cleanly here");
    say(f.db, s, "a conflict in the schedule spec");

    const result = await search(f, "q=patch+conflict");
    const ids = result.hits.map((h) => h.messageId).sort();
    assert.deepEqual(ids, [both.id, adjacent.id].sort(), "both terms required, order-free");
    assert.equal(result.rewritten, false);
    assert.equal(result.effectiveQuery, "patch conflict");
    assert.equal(result.count, 2);
    assert.equal(result.limit, DEFAULT_LIMIT);
    assert.equal(result.scope, null);
  } finally {
    f.close();
  }
});

Deno.test("a quoted phrase requires adjacency; the same words apart do not match", async () => {
  const f = fixture();
  try {
    const s = session(f.db, "phrases");
    const adjacent = say(f.db, s, "we hit a patch conflict on files.ts");
    say(f.db, s, "the patch applied but the conflict was elsewhere");

    const phrase = await search(f, `q=${encodeURIComponent('"patch conflict"')}`);
    assert.deepEqual(phrase.hits.map((h) => h.messageId), [adjacent.id]);
    assert.equal(phrase.rewritten, false);

    // The same two words unquoted find both — the phrase is what narrowed it.
    const loose = await search(f, "q=patch+conflict");
    assert.equal(loose.count, 2);
  } finally {
    f.close();
  }
});

Deno.test("ranking puts the denser match first", async () => {
  const f = fixture();
  try {
    const s = session(f.db, "ranking");
    // Written in the losing order on purpose: an implementation that returned
    // insertion order rather than rank would pass a test seeded the other way round.
    const sparse = say(
      f.db,
      s,
      "a long note about the schedule ticker, the queue drain, the artifact store, " +
        "the comment sidecar and the changes rail, which mentions patch exactly once " +
        "and otherwise talks about entirely unrelated machinery for several lines",
    );
    const dense = say(f.db, s, "patch, patch, patch — the patch grammar");

    const result = await search(f, "q=patch");
    assert.deepEqual(
      result.hits.map((h) => h.messageId),
      [dense.id, sparse.id],
      "bm25 ranks the short, term-dense message above the long one-mention message",
    );
  } finally {
    f.close();
  }
});

Deno.test("a hit carries the session, its title and kind, the role and the timestamp", async () => {
  const f = fixture();
  try {
    const root = session(f.db, "the spawner");
    const child = session(f.db, "review files.ts", "subagent", root.id);
    const m = say(f.db, child, "reticulating splines in the delegated branch", "supervisor");

    const result = await search(f, "q=reticulating");
    assert.equal(result.count, 1);
    const [hit] = result.hits;
    assert.equal(hit.messageId, m.id);
    assert.equal(hit.sessionId, child.id);
    assert.equal(hit.title, "review files.ts");
    assert.equal(hit.kind, "subagent");
    assert.equal(hit.collapsed, true, "a subagent opens only on drill-in (spec §4)");
    assert.equal(hit.originId, root.id);
    assert.equal(hit.role, "supervisor");
    assert.equal(hit.createdAt, m.createdAt);
    assert.match(hit.snippet, /reticulating/);
  } finally {
    f.close();
  }
});

Deno.test("?sessionId= scopes the search; an unknown one is a 404, not an empty answer", async () => {
  const f = fixture();
  try {
    const a = session(f.db, "session a");
    const b = session(f.db, "session b");
    const inA = say(f.db, a, "splines everywhere");
    say(f.db, b, "splines here too");

    assert.equal((await search(f, "q=splines")).count, 2);
    const scoped = await search(f, `q=splines&sessionId=${a.id}`);
    assert.deepEqual(scoped.hits.map((h) => h.messageId), [inA.id]);
    assert.equal(scoped.scope, a.id);

    const missing = await f.call(new Request(url("/search?q=splines&sessionId=nope")));
    assert.equal(missing.status, 404);
    assert.match((await missing.json()).error, /no session nope/);
  } finally {
    f.close();
  }
});

Deno.test("limit is clamped and honored; an empty query is a 400 that says what to type", async () => {
  const f = fixture();
  try {
    const s = session(f.db, "many");
    for (let i = 0; i < 5; i++) say(f.db, s, `splines number ${i}`);

    assert.equal((await search(f, "q=splines&limit=2")).count, 2);

    const empty = await f.call(new Request(url("/search?q=%20%20")));
    assert.equal(empty.status, 400);
    assert.match((await empty.json()).error, /search needs a query/);

    const bad = await f.call(new Request(url("/search?q=splines&limit=0")));
    assert.equal(bad.status, 400, "limit is validated at the boundary, not silently fixed");
    const why = (await bad.json()).error as string;
    assert.match(why, /invalid search \(limit: /, "the issue is named, not dumped as JSON");
    assert.doesNotMatch(why, /"code"/);

    // Absent entirely, which is how most people arrive here.
    const none = await f.call(new Request(url("/search")));
    assert.equal(none.status, 400);
    assert.match((await none.json()).error, /search needs a query/);
  } finally {
    f.close();
  }
});

Deno.test("a query FTS5 cannot parse is rewritten into phrases and the rewrite is reported", async () => {
  const f = fixture();
  try {
    const s = session(f.db, "punctuation");
    const m = say(f.db, s, "what's up with the foo-bar helper");
    say(f.db, s, "nothing to do with either word");

    // Bare `what's` and `foo-bar` are FTS5 syntax errors, not zero-result searches.
    const result = await search(f, `q=${encodeURIComponent("what's foo-bar")}`);
    assert.equal(result.rewritten, true);
    assert.equal(result.effectiveQuery, `"what's" AND "foo-bar"`);
    assert.deepEqual(result.hits.map((h) => h.messageId), [m.id]);

    // A valid operator query is never rewritten — the fallback only runs on a parse
    // failure, so `OR` keeps meaning OR.
    const operators = await search(f, "q=nothing+OR+helper");
    assert.equal(operators.rewritten, false);
    assert.equal(operators.count, 2);
  } finally {
    f.close();
  }
});

Deno.test("quoteQuery escapes an embedded quote instead of producing invalid syntax", () => {
  assert.equal(quoteQuery("a b"), `"a" AND "b"`);
  assert.equal(quoteQuery('say "hi"'), `"say" AND """hi"""`);
  assert.equal(quoteQuery("   "), "");
});

// ---- the index is never load-bearing ----------------------------------------

/**
 * Break the FTS table for real, through a second connection to the same file.
 *
 * A stubbed `indexMessage` would prove only that the wrapper catches what a stub
 * throws. This produces the actual failure — `no such table: messages_fts` from inside
 * the live handle's own statement — which is what a corrupted or half-created index
 * looks like in production.
 */
function breakFts(path: string): void {
  const side = new DatabaseSync(path);
  try {
    side.exec("DROP TABLE messages_fts");
  } finally {
    side.close();
  }
}

Deno.test("an FTS write failure does not break message insertion", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-search-" });
  const path = join(dir, "bough.db");
  const swallowed: unknown[] = [];
  const f = fixture({ path, onIndexError: () => swallowed.push(1) });
  try {
    const s = session(f.db, "a real session");
    // Healthy first, so the failure below is the only difference between the two.
    say(f.db, s, "before the index broke");
    assert.equal(f.raw.searchMessages("broke").length, 1);

    breakFts(path);

    // The whole point: the HTTP write path still lands the message.
    const res = await f.call(
      new Request(url(`/sessions/${s.id}/messages`), {
        method: "POST",
        body: JSON.stringify({ text: "after the index broke" }),
      }),
    );
    assert.equal(res.status, 202, await res.clone().text());
    const messages = f.db.messagesFor(s.id);
    assert.equal(messages.length, 2, "the message is persisted despite the index failure");
    assert.equal((messages[1].parts[0] as { text: string }).text, "after the index broke");

    // And direct inserts too — the guarantee is about `indexMessage`, not about HTTP.
    assert.doesNotThrow(() => say(f.db, s, "and again"));
    assert.equal(f.db.messagesFor(s.id).length, 3);

    // It failed quietly, but not invisibly: the failure is counted and reported.
    assert.equal(swallowed.length, 2);
    const health = indexHealth(f.db)!;
    assert.equal(health.failures, 2);
    assert.match(health.lastError!, /messages_fts/);
    assert.ok(health.lastFailureAt! > 0);

    // The unwrapped handle still throws — the wrapper is what holds the guarantee,
    // and this is the failure it is absorbing.
    assert.throws(() => f.raw.indexMessage(messages[1]), /messages_fts/);
  } finally {
    f.close();
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("a missing index is a 503 about the index, never a 400 about the query", async () => {
  const dir = await Deno.makeTempDir({ prefix: "bough-search-" });
  const path = join(dir, "bough.db");
  const f = fixture({ path });
  try {
    const s = session(f.db, "degraded");
    say(f.db, s, "indexed while healthy");
    breakFts(path);
    say(f.db, s, "written while broken and therefore unfindable");

    // `db.searchMessages` renders every failure of that statement as "not valid FTS5
    // syntax", so without the translation the user is told their word is malformed
    // while the real problem is that there is nothing to search.
    const during = await f.call(new Request(url("/search?q=indexed")));
    assert.equal(during.status, 503);
    const { error } = await during.json() as { error: string };
    assert.match(error, /search index is unavailable \(no such table: messages_fts\)/);
    assert.match(error, /restarting the server recreates it/);
    assert.doesNotMatch(error, /not valid FTS5 syntax/);

    // And a rebuild says the same thing rather than 500ing: it cannot create a table.
    const cannot = await f.call(new Request(url("/search/reindex"), { method: "POST" }));
    assert.equal(cannot.status, 503);

    // Restarting is what recreates it (the schema is applied at open). Same file,
    // fresh handle — exactly what the error tells the user to do.
    f.close();
    const restarted = fixture({ path });
    try {
      const before = await search(restarted, "q=unfindable");
      assert.equal(before.count, 0, "the index is empty until it is refilled");

      const repaired = await restarted.call(
        new Request(url("/search/reindex"), { method: "POST" }),
      );
      assert.equal(repaired.status, 200);
      assert.deepEqual(await repaired.json(), { rebuilt: true, messages: 2, sessions: 1 });

      const after = await search(restarted, "q=unfindable");
      assert.equal(after.count, 1, "the message written while broken is searchable again");
      assert.equal(after.index, undefined, "and the process starts with a clean counter");
    } finally {
      restarted.close();
    }
  } finally {
    await Deno.remove(dir, { recursive: true });
  }
});

Deno.test("a degraded index is reported on every search until a rebuild repairs it", async () => {
  const f = fixture();
  try {
    const s = session(f.db, "degraded");
    say(f.db, s, "indexed while healthy");

    // A write that fails for a reason a rebuild CAN repair — a locked, busy or
    // transiently unwritable index — swallowed by the wrapper on the insert path.
    const failing = searchSafeDb({
      ...f.db,
      indexMessage: () => {
        throw new Error("database is locked");
      },
      // Bound copies, because this stand-in is a plain object rather than the handle.
      createMessage: (m: Message) => f.db.createMessage(m),
      getSession: (id: string) => f.db.getSession(id),
      getMessage: (id: string) => f.db.getMessage(id),
      searchMessages: (q: string, o?: { sessionId?: string; limit?: number }) =>
        f.db.searchMessages(q, o),
      rebuildSearchIndex: () => f.raw.rebuildSearchIndex(),
      listSessions: () => f.db.listSessions(),
      messagesFor: (id: string) => f.db.messagesFor(id),
    } as unknown as Db, { onError: () => {} });
    say(failing, s, "written while the index was locked");

    const during = searchTranscripts(failing, "indexed");
    assert.equal(during.count, 1);
    assert.equal(during.index?.degraded, true);
    assert.equal(during.index?.failures, 1);
    assert.equal(during.index?.lastError, "database is locked");
    assert.match(during.index!.repair, /reindex/);
    assert.equal(
      searchTranscripts(failing, "locked").count,
      0,
      "the swallowed write is exactly the missing result the report warns about",
    );

    rebuildIndex(failing);
    const after = searchTranscripts(failing, "locked");
    assert.equal(after.index, undefined, "the counter is cleared by the repair");
    assert.equal(after.count, 1, "and the missing message is searchable again");
  } finally {
    f.close();
  }
});

Deno.test("the wrapper delegates every other method to the real handle", () => {
  const raw = openDb(":memory:");
  const db = searchSafeDb(raw);
  try {
    // A private-field brand check throws if a method runs with the wrapper as its
    // receiver, so this is the test that the binding in `searchSafeDb` is right.
    const root = session(db, "root");
    const child = db.createSession({
      id: crypto.randomUUID(),
      title: "child",
      kind: "fork",
      createdAt: clock++,
      parentId: root.id,
    });
    say(db, root, "ancestor prose");
    say(db, child, "own prose");

    assert.equal(db.getSession(root.id)?.title, "root");
    assert.equal(db.threadFor(child.id).length, 2);
    assert.deepEqual(db.ancestorChain(child.id).map((s) => s.id), [root.id, child.id]);
    assert.equal(db.listSessions().length, 2);
    assert.equal(db.busySessionIds().size, 0);
    db.setSessionTitle(root.id, "renamed");
    assert.equal(db.getSession(root.id)?.title, "renamed");
    assert.equal(db.searchMessages("prose").length, 2);
    // Repeated access returns the same bound copy rather than a fresh closure.
    assert.equal(db.getSession, db.getSession);
  } finally {
    raw.close();
  }
});

// ---- rebuild and recovery ---------------------------------------------------

Deno.test("a rebuild produces exactly what incremental indexing produced", () => {
  const f = fixture();
  try {
    const a = session(f.db, "a");
    const b = session(f.db, "b");
    say(f.db, a, "the patch grammar and its conflict rules");
    say(f.db, b, "another patch, another conflict");
    say(f.db, b, "no prose here indexes nothing relevant");

    const incremental = f.db.searchMessages("patch");
    const counts = rebuildIndex(f.db);
    assert.deepEqual(counts, { messages: 3, sessions: 2 });
    assert.deepEqual(f.db.searchMessages("patch"), incremental, "rebuild == incremental");
  } finally {
    f.close();
  }
});

Deno.test("recovered turn messages are indexed at boot, and a missing one is skipped", () => {
  const f = fixture();
  try {
    const s = session(f.db, "crashed mid-turn");
    // Exactly what a died-mid-stream turn leaves: a message persisted by the runner
    // that never reached the finish path where indexing happens.
    const stranded = f.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: s.id,
      role: "supervisor",
      parts: [{ type: "text", text: "half a sentence about the orphaned turn" }],
      pending: false,
      createdAt: clock++,
    });
    assert.equal(f.db.searchMessages("orphaned").length, 0, "unindexed to begin with");

    const indexed = indexRecoveredMessages(f.db, [
      { messageId: stranded.id },
      { messageId: "a message the recovery pass named but the database does not have" },
    ]);
    assert.equal(indexed, 1);
    assert.deepEqual(f.db.searchMessages("orphaned").map((h) => h.messageId), [stranded.id]);

    // Idempotent: running it twice does not double the rows.
    indexRecoveredMessages(f.db, [{ messageId: stranded.id }]);
    assert.equal(f.db.searchMessages("orphaned").length, 1);
  } finally {
    f.close();
  }
});

Deno.test("searchTranscripts skips a hit whose message is gone", () => {
  const f = fixture();
  try {
    const s = session(f.db, "drift");
    say(f.db, s, "a findable sentence");
    // Index drift: an FTS row pointing at a message the caller cannot fetch. The core
    // takes a `Db`, so the drift is injected rather than manufactured in SQL.
    const drifting: Db = new Proxy(f.db, {
      get(target, prop) {
        if (prop === "getMessage") return () => undefined;
        const value = Reflect.get(target, prop, target);
        return typeof value === "function" ? value.bind(target) : value;
      },
    }) as Db;
    const result = searchTranscripts(drifting, "findable");
    assert.equal(result.count, 0);
    assert.deepEqual(result.hits, []);
  } finally {
    f.close();
  }
});
