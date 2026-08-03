/**
 * Keyword search over transcripts — the whole of cross-session recall (spec §17:
 * SQLite FTS, no embeddings, no vector index).
 *
 * THE INVARIANT THIS MODULE HOLDS: **the search index is never load-bearing.** A
 * failure to index must never fail the write that triggered it. Indexing runs on the
 * insert path — `server/sessions.ts`, `turn/runner.ts`, `history/branch.ts`,
 * `agents/*`, `schedules.ts` all call `db.indexMessage` as they persist — which is
 * what keeps the index current with no background job to babysit and no lazy catch-up
 * pass that makes the first search of the day slow. The price of being on that path is
 * that a broken `messages_fts` becomes a broken `POST /sessions/:id/messages`, and
 * losing a user's message to a search-index error is not a trade anyone would take.
 *
 * `searchSafeDb` is where that is paid, once, at the seam: it wraps the `Db` handed to
 * the app so `indexMessage` reports and swallows instead of throwing. Most call sites
 * already guard themselves; wrapping is what makes the guarantee hold for the one that
 * does not, and for every future one, without a rule everybody has to remember.
 *
 * WHAT A SWALLOWED ERROR COSTS, AND WHY THE COUNTER EXISTS. Silent degradation is the
 * bad failure mode for search: results that are quietly missing look exactly like
 * results that do not exist, and nothing ever repairs them, because the write they
 * belonged to is long gone. So the wrapper counts what it swallowed and `GET /search`
 * says so on every response that follows, pointing at `POST /search/reindex` — the
 * repair. A search index is the one subsystem allowed to fail quietly; it is not
 * allowed to fail invisibly.
 *
 * QUERY SYNTAX IS THE USER'S, NOT OURS. The query goes to FTS5 verbatim, so `"a
 * phrase"`, `OR`, `NOT`, `NEAR` and `pref*` all work as documented. Bare words are
 * FTS5's implicit AND: `patch conflict` finds messages containing both, anywhere.
 * Punctuation, though, is FTS5 syntax — `what's up` and `foo-bar` are hard errors, not
 * zero-result searches — so a query that fails to parse is retried once with every
 * whitespace-separated chunk quoted into a phrase, and the response reports the
 * rewrite in `effectiveQuery`. Failing an ordinary human query with a parser message
 * would be indefensible; rewriting one without saying so would be worse.
 *
 * HITS CARRY THEIR CONTEXT. A bare message id is useless to a human: a hit is worth
 * showing only if you can tell which conversation it came from, roughly when, and what
 * was said. So each hit carries the session's id, title and kind, the message's role
 * and timestamp, and the FTS snippet. `collapsed` restates the derived-visibility rule
 * (spec §4) locally rather than importing `server/sessions.ts`: search legitimately
 * reaches into delegated sessions, which never appear in a top-level listing, and a
 * client needs to know a hit belongs to a branch it must drill into to open.
 */
import { BadRequestError, HttpError, NotFoundError } from "../errors.ts";
import type { Message, Session, SessionKind } from "../schema/parts.ts";
import { COLLAPSED_KINDS } from "../schema/parts.ts";
import { SearchQuery } from "../schema/requests.ts";
import type { Db, SearchHit } from "../types.ts";
import { type Handler, json } from "./http.ts";

// ---- shapes -----------------------------------------------------------------

/** Hits per page when the caller does not say. Small: this is a picker, not an export. */
export const DEFAULT_LIMIT = 20;
/** The ceiling `schema/requests.ts` also enforces on the wire. */
export const MAX_LIMIT = 200;

/**
 * What an empty search says. One sentence of syntax, because the alternative a user
 * meets first is a Zod issue array about a zero-length string — technically the same
 * fact, useless as an answer (spec §6: error text is a product surface).
 */
const NEEDS_A_QUERY = "search needs a query — GET /search?q=<words>. Bare words are " +
  'ANDed; quote a phrase as "like this"; OR, NOT, NEAR and pref* work too.';

/** The kinds that collapse under their `originId` and open only on drill-in (spec §4). */
// The canonical list lives in `schema/parts.ts`; this was a local copy of it.
// See the import at the top of the file.

/** One hit, with enough around it to be worth rendering. */
export interface SearchResultHit {
  messageId: string;
  sessionId: string;
  /** The owning session's title — what a human recognizes the conversation by. */
  title: string;
  kind: SessionKind;
  /** True when the session surfaces only under `originId` (spec §4). */
  collapsed: boolean;
  /** Where to drill in from, for a collapsed session. */
  originId?: string;
  /** user | supervisor | system — "did I say this, or did the agent?" */
  role: Message["role"];
  /** The matched excerpt, FTS snippet markers already resolved. */
  snippet: string;
  /** Epoch ms; the message's own timestamp, not the session's. */
  createdAt: number;
}

/** What the index has swallowed in this process. Absent counters mean a healthy run. */
export interface IndexHealth {
  failures: number;
  lastError: string | null;
  lastFailureAt: number | null;
}

export interface SearchResult {
  /** The query as typed, trimmed. */
  query: string;
  /** What FTS5 actually ran — differs from `query` only after a rewrite. */
  effectiveQuery: string;
  /** True when `query` did not parse and was rewritten into quoted phrases. */
  rewritten: boolean;
  /** The session the search was scoped to, or null for the whole forest. */
  scope: string | null;
  limit: number;
  count: number;
  hits: SearchResultHit[];
  /**
   * Present only when this process has swallowed an indexing error, in which case
   * results may be incomplete and `POST /search/reindex` is the repair.
   */
  index?: IndexHealth & { degraded: true; repair: string };
}

// ---- the safety wrapper -----------------------------------------------------

/** Where the health record hides on a wrapped handle. Module-private on purpose. */
const HEALTH = Symbol("bough.searchIndexHealth");

interface Healed {
  [HEALTH]?: { health: IndexHealth; heal: () => void };
}

export interface SearchSafeOptions {
  /** Where a swallowed indexing error is reported. Default logs it. */
  onError?: (error: unknown, message: Message) => void;
  /** Injected clock for `lastFailureAt`. Absent = `Date.now`. */
  now?: () => number;
}

/**
 * Wrap a `Db` so `indexMessage` can never fail its caller.
 *
 * Every other method delegates untouched, so this is installable at boot over the one
 * real handle and nothing downstream can tell the difference — which is the point:
 * the guarantee has to hold for call sites this module has never heard of.
 *
 * WHY A PROXY AND WHY THE BINDING. `SqliteDb` keeps its statement cache and connection
 * in `#private` fields, and a private field is a brand check on the *receiver*: called
 * with the wrapper as `this`, every method would throw `TypeError: Cannot read private
 * member`. So each method is read with the target as receiver and bound to the target
 * before it is handed out — and the bound copies are cached, because this handle sits
 * under every database call in the server and allocating a closure per property read
 * would be a real cost for a wrapper that exists to be invisible.
 */
export function searchSafeDb(db: Db, opts: SearchSafeOptions = {}): Db {
  const now = opts.now ?? Date.now;
  const report = opts.onError ??
    ((err: unknown, m: Message) =>
      console.error(
        `search index write failed for message ${m.id} (search results may be ` +
          `incomplete; POST /search/reindex repairs it):`,
        err,
      ));

  const health: IndexHealth = { failures: 0, lastError: null, lastFailureAt: null };
  const heal = (): void => {
    health.failures = 0;
    health.lastError = null;
    health.lastFailureAt = null;
  };

  const indexMessage = (m: Message): void => {
    try {
      db.indexMessage(m);
    } catch (err) {
      health.failures++;
      health.lastError = err instanceof Error ? err.message : String(err);
      health.lastFailureAt = now();
      // The report itself must not throw past the write it is protecting.
      try {
        report(err, m);
      } catch { /* a reporter that fails is not the caller's problem */ }
    }
  };

  const bound = new Map<PropertyKey, unknown>();
  return new Proxy(db, {
    get(target, prop) {
      if (prop === HEALTH) return { health, heal };
      if (prop === "indexMessage") return indexMessage;
      const cached = bound.get(prop);
      if (cached !== undefined) return cached;
      const value = Reflect.get(target, prop, target);
      const out = typeof value === "function" ? value.bind(target) : value;
      if (typeof value === "function") bound.set(prop, out);
      return out;
    },
  }) as Db;
}

/** The swallowed-error record, when `db` came through `searchSafeDb`. */
export function indexHealth(db: Db): IndexHealth | undefined {
  return (db as Healed)[HEALTH]?.health;
}

/** Clear the record — called after a rebuild has actually repaired the drift. */
function healIndex(db: Db): void {
  (db as Healed)[HEALTH]?.heal();
}

// ---- index maintenance ------------------------------------------------------

/**
 * Re-index the messages an orphaned turn left behind, and answer how many.
 *
 * A turn that died mid-stream never reached the finish path that indexes its message
 * (`turn/runner.ts`), so everything the supervisor had already said in it would be
 * unsearchable forever — and boot recovery is the one moment those messages are known,
 * closed and enumerated (`turn/state.ts`). Idempotent like every other index write, so
 * a message that *was* indexed simply gets the same rows back.
 */
export function indexRecoveredMessages(
  db: Db,
  recovered: readonly { messageId: string }[],
): number {
  let indexed = 0;
  for (const { messageId } of recovered) {
    try {
      const message = db.getMessage(messageId);
      if (!message) continue;
      db.indexMessage(message);
      indexed++;
    } catch (err) {
      // Recovery is best-effort and runs before the listener binds: a search index
      // that cannot be written must not stop the server from starting.
      console.error(`failed to index recovered message ${messageId}:`, err);
    }
  }
  return indexed;
}

/**
 * Rebuild the whole index from `messages` and report how many messages it covers.
 *
 * The rebuild itself is `db.rebuildSearchIndex()`, which clears and re-projects
 * through the same function the insert path uses — that shared projection is what
 * makes a rebuild produce results identical to incremental indexing (plan T8.9). The
 * count is walked separately because that guarantee is worth more than saving a pass:
 * a second projection here to count as it went would be a second thing to keep in sync.
 *
 * Deliberately synchronous and deliberately unguarded. This is the repair path, asked
 * for explicitly by a human; a rebuild that failed and answered "ok" would leave them
 * believing search had been fixed.
 */
export function rebuildIndex(db: Db): { messages: number; sessions: number } {
  try {
    db.rebuildSearchIndex();
  } catch (err) {
    // The one failure worth translating: a rebuild cannot create the table it writes
    // into, so "no such table" needs the restart-then-reindex sentence rather than a
    // 500 that reads as a bug in the rebuild.
    if (namesTheIndex(err)) throw new SearchIndexUnavailableError(sqliteCause(err));
    throw err;
  }
  healIndex(db);
  const sessions = db.listSessions();
  let messages = 0;
  for (const session of sessions) messages += db.messagesFor(session.id).length;
  return { messages, sessions: sessions.length };
}

// ---- query ------------------------------------------------------------------

/**
 * Rewrite a query that FTS5 refused to parse into one it will accept.
 *
 * Each whitespace-separated chunk becomes a quoted phrase, joined by AND. That keeps
 * the reading a human intends — every term must appear — while neutralizing the
 * punctuation FTS5 treats as syntax: `what's` becomes the two-token phrase it was
 * indexed as, `foo-bar` matches the hyphenated text, and a stray `"` is escaped by
 * doubling rather than swallowed. Operators are lost, which is correct: this only runs
 * for a query that was not valid operator syntax in the first place.
 */
export function quoteQuery(query: string): string {
  return query
    .split(/\s+/)
    .filter((chunk) => chunk.length > 0)
    .map((chunk) => `"${chunk.replaceAll('"', '""')}"`)
    .join(" AND ");
}

/** True when the FTS parser, not the corpus, rejected the query. */
function isSyntaxError(err: unknown): boolean {
  return err instanceof BadRequestError && !namesTheIndex(err);
}

/**
 * True when the failure is the index itself rather than the query.
 *
 * `db.searchMessages` renders EVERY error from that statement as "not valid FTS5
 * syntax" (`db/db.ts`), which is right for the case it was written for and wrong for
 * a missing or corrupt `messages_fts`: the user is told the words they typed are
 * malformed while the real problem is that there is nothing to search. Sniffing the
 * SQLite text is the only discriminator the port exposes, and getting this wrong in
 * either direction only changes which correct-status error is reported, never whether
 * one is.
 */
function namesTheIndex(err: unknown): boolean {
  const text = err instanceof Error ? err.message : String(err);
  return /no such table: messages_fts|no such module: fts5|database disk image is malformed/i
    .test(text);
}

/** The SQLite sentence inside the port's wrapped message, for the report below. */
function sqliteCause(err: unknown): string {
  const text = err instanceof Error ? err.message : String(err);
  return /(no such table: messages_fts|no such module: fts5|database disk image is malformed)/i
    .exec(text)?.[1] ?? text;
}

/**
 * 503 — the index is gone, not the query. Named separately from a 400 because the fix
 * is different in kind: nothing the user retypes will help, and a rebuild cannot create
 * a table either (the schema is applied at open, and `db/` owns the SQL). Error text is
 * a product surface (spec §6): this one names what failed, the state that caused it,
 * and the move that resolves it.
 */
export class SearchIndexUnavailableError extends HttpError {
  constructor(cause: string) {
    super(
      503,
      `the search index is unavailable (${cause}). The transcripts themselves are ` +
        `intact — messages are stored in \`messages\`, and the index is a projection ` +
        `of them. \`messages_fts\` is created when the database is opened, so ` +
        `restarting the server recreates it; POST /search/reindex then refills it from ` +
        `the stored messages.`,
    );
  }
}

/**
 * Run one search. The pure-ish core the route is a wrapper over: it takes a `Db` and
 * returns data, so it is testable without a request.
 */
export function searchTranscripts(
  db: Db,
  query: string,
  opts: { sessionId?: string; limit?: number } = {},
): SearchResult {
  const trimmed = query.trim();
  if (trimmed.length === 0) throw new BadRequestError(NEEDS_A_QUERY);
  const limit = Math.min(Math.max(Math.trunc(opts.limit ?? DEFAULT_LIMIT), 1), MAX_LIMIT);

  if (opts.sessionId !== undefined && !db.getSession(opts.sessionId)) {
    throw new NotFoundError(
      `no session ${opts.sessionId} — drop ?sessionId= to search every transcript.`,
    );
  }

  let effectiveQuery = trimmed;
  let rewritten = false;
  let raw: SearchHit[];
  try {
    raw = db.searchMessages(trimmed, { sessionId: opts.sessionId, limit });
  } catch (err) {
    // A parse failure is retried as quoted phrases; a broken index is reported as
    // itself; anything else is the database talking and belongs to the caller
    // unchanged.
    if (namesTheIndex(err)) throw new SearchIndexUnavailableError(sqliteCause(err));
    if (!isSyntaxError(err)) throw err;
    effectiveQuery = quoteQuery(trimmed);
    rewritten = true;
    if (effectiveQuery.length === 0) throw err;
    raw = db.searchMessages(effectiveQuery, { sessionId: opts.sessionId, limit });
  }

  const titles = new Map<string, Session | undefined>();
  const hits: SearchResultHit[] = [];
  for (const hit of raw) {
    // A hit whose message is gone is index drift, not a result: showing a snippet
    // with nothing to open would be worse than showing one fewer hit.
    const message = db.getMessage(hit.messageId);
    if (!message) continue;
    if (!titles.has(hit.sessionId)) titles.set(hit.sessionId, db.getSession(hit.sessionId));
    const session = titles.get(hit.sessionId);
    hits.push({
      messageId: hit.messageId,
      sessionId: hit.sessionId,
      title: session?.title ?? "(unknown session)",
      kind: session?.kind ?? "root",
      collapsed: session ? COLLAPSED_KINDS.includes(session.kind) : false,
      ...(session?.originId ? { originId: session.originId } : {}),
      role: message.role,
      snippet: hit.snippet,
      createdAt: hit.createdAt,
    });
  }

  const health = indexHealth(db);
  return {
    query: trimmed,
    effectiveQuery,
    rewritten,
    scope: opts.sessionId ?? null,
    limit,
    count: hits.length,
    hits,
    ...(health && health.failures > 0
      ? {
        index: {
          ...health,
          degraded: true as const,
          repair: "POST /search/reindex rebuilds the index from the stored messages.",
        },
      }
      : {}),
  };
}

// ---- routes -----------------------------------------------------------------

/**
 * `GET /search?q=…&sessionId=…&limit=…` — keyword search over every transcript.
 *
 * Validated through the same Zod schema the rest of the wire uses, with the numeric
 * `limit` coerced first: query strings are strings, and a schema that took `"20"` as
 * valid would have `limit` typed as a number and holding text.
 */
export const searchH: Handler = (req, ctx, _params) => {
  const url = new URL(req.url);
  const q = url.searchParams.get("q") ?? "";
  // Answered before the schema, because the schema's version of "you typed nothing"
  // is an issue array and this is the single most likely way to arrive here.
  if (q.trim().length === 0) throw new BadRequestError(NEEDS_A_QUERY);
  const rawLimit = url.searchParams.get("limit");
  const sessionId = url.searchParams.get("sessionId");
  const parsed = SearchQuery.safeParse({
    q,
    ...(sessionId ? { sessionId } : {}),
    ...(rawLimit === null ? {} : { limit: Number(rawLimit) }),
  });
  if (!parsed.success) {
    // Flattened to `path: message`, one per line. The raw `error.message` is a JSON
    // dump of the issue array, which is a debugger's view pasted at a user.
    const why = parsed.error.issues
      .map((issue) => `${issue.path.join(".") || "query"}: ${issue.message}`)
      .join("; ");
    throw new BadRequestError(
      `invalid search (${why}) — GET /search?q=<words>` +
        `[&sessionId=<id>][&limit=1..${MAX_LIMIT}]`,
    );
  }
  return json(searchTranscripts(ctx.db, parsed.data.q, {
    sessionId: parsed.data.sessionId,
    limit: parsed.data.limit,
  }));
};

/**
 * `POST /search/reindex` — rebuild the index from the stored messages.
 *
 * The repair for the drift a swallowed indexing error leaves behind, and the reason
 * swallowing one is defensible at all. `messages` is the transcript corpus, not a
 * count of index rows: a message with no prose contributes nothing to index, so the
 * two are not the same number and reporting FTS internals here would invite reading a
 * legitimate difference as a bug.
 */
export const reindexH: Handler = (_req, ctx, _params) => {
  const { messages, sessions } = rebuildIndex(ctx.db);
  return json({ rebuilt: true, messages, sessions });
};
