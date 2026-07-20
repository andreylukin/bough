/**
 * SQLite persistence for sessions, messages, and the two side-channels (net_events,
 * snapshots). Tree history lives here: sessions form a parent_id forest and a session's
 * "thread" is the concatenation of messages along the root→self path (see threadFor).
 *
 * Driver decision: plain SQL over Deno's built-in `node:sqlite` (DatabaseSync),
 * wrapped in a thin typed layer below. We deliberately skip Drizzle — on Deno its
 * SQLite story means pulling a second native driver (better-sqlite3/libsql) when the
 * runtime already ships one, and our query surface is a handful of statements that read
 * clearer as SQL. The Zod schemas in schema/parts.ts already give us the type safety
 * Drizzle would. Net/snapshot owners extend the minimal columns here later.
 *
 * Storage: JSON columns for message `parts`; booleans as 0/1; timestamps as epoch ms.
 * DB file at ~/.bough/bough.db, overridable via BOUGH_DB (":memory:" for tests).
 */
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { homedir } from "node:os";
import { mkdirSync } from "node:fs";
import type { Message, NetRequest, Part, Role, Session, SessionKind } from "../schema/parts.ts";

const SCHEMA = `
CREATE TABLE IF NOT EXISTS sessions (
  id          TEXT PRIMARY KEY,
  parent_id   TEXT REFERENCES sessions(id),
  title       TEXT NOT NULL,
  kind        TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  workspace   TEXT,                   -- read-write root for this session; null = BOUGH_WORKSPACE/cwd
  base        TEXT,                   -- persisted snapshot/git base commit, captured on the first turn
  origin_id         TEXT,             -- lineage: session this fork/compaction branched from (null for root/plain)
  origin_message_id TEXT,             -- lineage: the fork-at message / compaction span-end message
  archived_at INTEGER,                -- soft delete: archived sessions leave the sidebar, rows stay
  deprecated_at INTEGER,              -- branch hidden by default in the tree views (toggle to show)
  context_tokens INTEGER,             -- last turn's prompt size (context meter)
  output_tokens  INTEGER,             -- cumulative output tokens across the session
  input_tokens   INTEGER,             -- cumulative input tokens across the session (cost)
  cached_tokens  INTEGER,             -- last LLM round: prompt tokens read from / written to the provider cache
  last_llm_at    INTEGER,             -- epoch ms the last LLM round finished (cache-warmth clock)
  model       TEXT                    -- per-session model override; null = the global default
);
CREATE TABLE IF NOT EXISTS messages (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id),
  role        TEXT NOT NULL,
  parts       TEXT NOT NULL,          -- JSON Part[]
  pending     INTEGER NOT NULL,       -- 0/1
  created_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS messages_session ON messages(session_id, created_at);
-- Side-channels: minimal columns; the net-gate / snapshot owners extend these.
CREATE TABLE IF NOT EXISTS net_events (
  id           TEXT PRIMARY KEY,
  session_id   TEXT,
  host         TEXT NOT NULL,
  verb         TEXT,
  action       TEXT NOT NULL,
  verdict      TEXT NOT NULL,
  reason       TEXT,
  requested_by TEXT,
  fields       TEXT,                   -- JSON facet fields (classifier's parsed view)
  ts           INTEGER NOT NULL
);
-- Per-session egress policy overrides (Claw Patrol). A session with no row inherits
-- the nearest ancestor's row, falling back to the global ~/.bough/net/policy.json.
CREATE TABLE IF NOT EXISTS net_policies (
  session_id  TEXT PRIMARY KEY REFERENCES sessions(id),
  config      TEXT NOT NULL,          -- JSON NetConfig
  updated_at  INTEGER NOT NULL
);
-- The supervisor state machine: one row per in-flight (or finished) turn. The
-- runner checkpoints the step after each API round + each tool result so a restart
-- can find turns still marked running and orphan them (see turns.ts).
CREATE TABLE IF NOT EXISTS turns (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id),
  message_id  TEXT NOT NULL REFERENCES messages(id),  -- the pending supervisor msg
  status      TEXT NOT NULL,          -- running | done | error | orphaned
  step        TEXT NOT NULL,          -- last checkpoint (human-readable)
  updated_at  INTEGER NOT NULL,
  first_output_at INTEGER             -- when the user first SAW anything (metrics)
);
CREATE INDEX IF NOT EXISTS turns_status ON turns(status);
-- Message embeddings for recall search (local embedder — see recall.ts). Vectors are
-- unit-normalized Float32 blobs; dim=0 marks a message with nothing to embed so the
-- indexer doesn't retry it forever.
CREATE TABLE IF NOT EXISTS message_embeddings (
  message_id  TEXT PRIMARY KEY REFERENCES messages(id),
  session_id  TEXT NOT NULL,
  dim         INTEGER NOT NULL,
  vector      BLOB
);
`;

// ---- row <-> domain mapping ------------------------------------------------

type SessionRow = {
  id: string;
  parent_id: string | null;
  title: string;
  kind: string;
  created_at: number;
  workspace: string | null;
  base: string | null;
  origin_id: string | null;
  origin_message_id: string | null;
  deprecated_at: number | null;
  context_tokens: number | null;
  cached_tokens: number | null;
  last_llm_at: number | null;
  model: string | null;
  effort: string | null;
  draft: string | null;
};

/**
 * The runtime (non-wire) facts a session carries for the turn runner: its explicit
 * workspace (null = fall back to BOUGH_WORKSPACE/cwd) and its persisted snapshot base.
 * Kept off the wire `Session` type so the UI mirror in schema/parts.ts is untouched.
 */
export interface SessionRuntime {
  workspace: string | null;
  base: string | null;
}
type MessageRow = {
  id: string;
  session_id: string;
  role: string;
  parts: string;
  pending: number;
  created_at: number;
};

export type TurnStatus = "running" | "done" | "error" | "orphaned" | "interrupted";
export interface Turn {
  id: string;
  sessionId: string;
  messageId: string;
  status: TurnStatus;
  step: string;
  updatedAt: number;
  /** When the turn's first output (delta or part) reached the UI — see metrics.ts. */
  firstOutputAt: number | null;
}
type TurnRow = {
  id: string;
  session_id: string;
  message_id: string;
  status: string;
  step: string;
  updated_at: number;
  first_output_at: number | null;
};

function toTurn(r: TurnRow): Turn {
  return {
    id: r.id,
    sessionId: r.session_id,
    messageId: r.message_id,
    status: r.status as TurnStatus,
    step: r.step,
    updatedAt: r.updated_at,
    firstOutputAt: r.first_output_at,
  };
}

function toSession(r: SessionRow): Session {
  return {
    id: r.id,
    parentId: r.parent_id,
    title: r.title,
    kind: r.kind as SessionKind,
    createdAt: r.created_at,
    // Only surface optional fields when set, so responses stay byte-identical otherwise.
    ...(r.workspace ? { workspace: r.workspace } : {}),
    ...(r.origin_id ? { originId: r.origin_id } : {}),
    ...(r.origin_message_id ? { originMessageId: r.origin_message_id } : {}),
    ...(r.deprecated_at != null ? { deprecatedAt: r.deprecated_at } : {}),
    ...(r.model ? { model: r.model } : {}),
    ...(r.effort ? { effort: r.effort } : {}),
    // Prompt-cache visibility: last prompt size, its cached share, and when the
    // last LLM round finished (the client derives warm/cold from this + the TTL).
    ...(r.context_tokens != null ? { contextTokens: r.context_tokens } : {}),
    ...(r.cached_tokens != null ? { cachedTokens: r.cached_tokens } : {}),
    ...(r.last_llm_at != null ? { lastLlmAt: r.last_llm_at } : {}),
    ...(r.draft != null ? { draft: r.draft } : {}),
  };
}

function toMessage(r: MessageRow): Message {
  return {
    id: r.id,
    sessionId: r.session_id,
    role: r.role as Role,
    parts: JSON.parse(r.parts) as Part[],
    pending: r.pending === 1,
    createdAt: r.created_at,
  };
}

// ---- the typed handle ------------------------------------------------------

export class Db {
  #db: DatabaseSync;

  constructor(path: string) {
    this.#db = new DatabaseSync(path);
    this.#db.exec("PRAGMA foreign_keys = ON");
    this.#db.exec(SCHEMA);
    this.#migrate();
  }

  // Idempotent column adds for DB files created before these columns existed.
  // CREATE TABLE IF NOT EXISTS won't add them, so ALTER and swallow the dup error.
  #migrate(): void {
    for (
      const col of [
        "workspace TEXT",
        "base TEXT",
        "origin_id TEXT",
        "origin_message_id TEXT",
        "archived_at INTEGER",
        "deprecated_at INTEGER",
        "context_tokens INTEGER",
        "output_tokens INTEGER",
        "input_tokens INTEGER",
        "cached_tokens INTEGER",
        "cache_read_total INTEGER",
        "cache_write_total INTEGER",
        "cost_usd REAL",
        "last_llm_at INTEGER",
        "model TEXT",
        "effort TEXT",
        "draft TEXT",
      ]
    ) {
      try {
        this.#db.exec(`ALTER TABLE sessions ADD COLUMN ${col}`);
      } catch {
        // column already exists
      }
    }
    try {
      this.#db.exec(`ALTER TABLE net_events ADD COLUMN fields TEXT`);
    } catch {
      // column already exists
    }
    try {
      this.#db.exec(`ALTER TABLE turns ADD COLUMN first_output_at INTEGER`);
    } catch {
      // column already exists
    }
  }

  close(): void {
    this.#db.close();
  }

  // sessions ----------------------------------------------------------------

  createSession(s: Session): Session {
    this.#db
      .prepare(
        `INSERT INTO sessions (id, parent_id, title, kind, created_at, workspace, origin_id, origin_message_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        s.id,
        s.parentId,
        s.title,
        s.kind,
        s.createdAt,
        s.workspace ?? null,
        s.originId ?? null,
        s.originMessageId ?? null,
      );
    return s;
  }

  getSession(id: string): Session | undefined {
    const r = this.#db.prepare(`SELECT * FROM sessions WHERE id = ?`).get(id) as
      | SessionRow
      | undefined;
    return r && toSession(r);
  }

  /** The turn runner's runtime view: explicit workspace + persisted base (both nullable). */
  getSessionRuntime(id: string): SessionRuntime {
    const r = this.#db
      .prepare(`SELECT workspace, base FROM sessions WHERE id = ?`)
      .get(id) as { workspace: string | null; base: string | null } | undefined;
    return { workspace: r?.workspace ?? null, base: r?.base ?? null };
  }

  setSessionTitle(id: string, title: string): void {
    this.#db.prepare(`UPDATE sessions SET title = ? WHERE id = ?`).run(title, id);
  }

  setSessionWorkspace(id: string, workspace: string): void {
    this.#db.prepare(`UPDATE sessions SET workspace = ? WHERE id = ?`).run(workspace, id);
  }

  /** Set (handoff) or clear (first post) a session's drafted opening prompt. */
  setSessionDraft(id: string, draft: string | null): void {
    this.#db.prepare(`UPDATE sessions SET draft = ? WHERE id = ?`).run(draft, id);
  }

  /** Per-session model override; null clears back to the global default. */
  setSessionModel(id: string, model: string | null): void {
    this.#db.prepare(`UPDATE sessions SET model = ? WHERE id = ?`).run(model, id);
  }

  /** Per-session thinking-depth override; null clears back to the global default. */
  setSessionEffort(id: string, effort: string | null): void {
    this.#db.prepare(`UPDATE sessions SET effort = ? WHERE id = ?`).run(effort, id);
  }

  /** Record the base commit captured on a session's first turn (see supervisor/workspace.ts). */
  setSessionBase(id: string, base: string): void {
    this.#db.prepare(`UPDATE sessions SET base = ? WHERE id = ?`).run(base, id);
  }

  /** Unarchived sessions, newest first (the sidebar list). */
  listSessions(): Session[] {
    const rows = this.#db
      .prepare(`SELECT * FROM sessions WHERE archived_at IS NULL ORDER BY created_at DESC`)
      .all() as SessionRow[];
    return rows.map(toSession);
  }

  /**
   * Token usage: last turn's prompt size (context meter) + cumulative output and
   * cumulative input across the session (cost accounting — input dominates cost
   * because every round re-sends the whole thread).
   */
  setSessionUsage(
    id: string,
    contextTokens: number,
    outputTokens: number,
    inputTokens: number,
    costUsd = 0,
  ): void {
    this.#db
      .prepare(
        `UPDATE sessions SET context_tokens = ?, output_tokens = ?, input_tokens = ?,
           cost_usd = ? WHERE id = ?`,
      )
      .run(contextTokens, outputTokens, inputTokens, costUsd, id);
  }

  /**
   * Last LLM round's cache stats (cached prompt share + finish time, the warmth
   * clock) plus cumulative cache read/write totals across the session — the
   * discounted share of input_tokens (reads bill ~0.1x, writes ~1.25x).
   */
  setSessionCache(
    id: string,
    cachedTokens: number,
    lastLlmAt: number,
    cacheReadTotal = 0,
    cacheWriteTotal = 0,
  ): void {
    this.#db
      .prepare(
        `UPDATE sessions SET cached_tokens = ?, last_llm_at = ?,
           cache_read_total = COALESCE(cache_read_total, 0) + ?,
           cache_write_total = COALESCE(cache_write_total, 0) + ?
         WHERE id = ?`,
      )
      .run(cachedTokens, lastLlmAt, cacheReadTotal, cacheWriteTotal, id);
  }

  sessionUsage(id: string): {
    contextTokens: number;
    outputTokens: number;
    inputTokens: number;
    cachedTokens: number;
    cacheReadTotal: number;
    cacheWriteTotal: number;
    costUsd: number;
    lastLlmAt: number | null;
  } {
    const r = this.#db
      .prepare(
        `SELECT context_tokens, output_tokens, input_tokens, cached_tokens,
                cache_read_total, cache_write_total, cost_usd, last_llm_at
         FROM sessions WHERE id = ?`,
      )
      .get(id) as {
        context_tokens: number | null;
        output_tokens: number | null;
        input_tokens: number | null;
        cached_tokens: number | null;
        cache_read_total: number | null;
        cache_write_total: number | null;
        cost_usd: number | null;
        last_llm_at: number | null;
      } | undefined;
    return {
      contextTokens: r?.context_tokens ?? 0,
      outputTokens: r?.output_tokens ?? 0,
      inputTokens: r?.input_tokens ?? 0,
      cachedTokens: r?.cached_tokens ?? 0,
      cacheReadTotal: r?.cache_read_total ?? 0,
      cacheWriteTotal: r?.cache_write_total ?? 0,
      costUsd: r?.cost_usd ?? 0,
      lastLlmAt: r?.last_llm_at ?? null,
    };
  }

  /**
   * Cumulative usage for a session PLUS its whole subagent subtree (cost rollup).
   * Follows origin_id but only through kind='subagent' rows, so forks/compactions
   * don't count into a session's spend. Includes archived descendants — they cost
   * money too. `sessions` = descendant count (0 for a leaf).
   */
  treeUsage(
    id: string,
  ): { inputTokens: number; outputTokens: number; costUsd: number; sessions: number } {
    const r = this.#db
      .prepare(
        `WITH RECURSIVE tree(id) AS (
           SELECT id FROM sessions WHERE id = ?
           UNION ALL
           SELECT s.id FROM sessions s JOIN tree t ON s.origin_id = t.id
           WHERE s.kind = 'subagent'
         )
         SELECT COUNT(*) - 1 AS descendants,
                COALESCE(SUM(input_tokens), 0) AS input,
                COALESCE(SUM(output_tokens), 0) AS output,
                COALESCE(SUM(cost_usd), 0) AS cost
         FROM sessions WHERE id IN (SELECT id FROM tree)`,
      )
      .get(id) as { descendants: number; input: number; output: number; cost: number };
    return {
      inputTokens: r.input,
      outputTokens: r.output,
      costUsd: r.cost,
      sessions: r.descendants,
    };
  }

  /** Sessions with a turn in flight (any message still pending) — sidebar busy dots. */
  busySessionIds(): Set<string> {
    const rows = this.#db
      .prepare(`SELECT DISTINCT session_id FROM messages WHERE pending = 1`)
      .all() as { session_id: string }[];
    return new Set(rows.map((r) => r.session_id));
  }

  /**
   * Soft-delete: the session leaves the sidebar but the row (and its messages,
   * lineage, and descendants' ancestor chains) stays intact.
   */
  archiveSession(id: string): void {
    this.#db.prepare(`UPDATE sessions SET archived_at = ? WHERE id = ?`).run(Date.now(), id);
  }

  /** Deprecate (hide-by-default in the tree) or un-deprecate a branch. */
  setDeprecated(id: string, on: boolean): void {
    this.#db.prepare(`UPDATE sessions SET deprecated_at = ? WHERE id = ?`)
      .run(on ? Date.now() : null, id);
  }

  /**
   * Hard-delete sessions archived before `cutoff` (epoch ms) and all their rows —
   * the long-term purge behind soft-archive. Returns the number of sessions removed.
   * FK enforcement is toggled off for the sweep so child rows and inter-session
   * parent pointers don't dictate a delete order; everything referencing a purged
   * session is being removed or is a survivor whose pointer is nulled.
   */
  purgeArchivedBefore(cutoff: number): number {
    const ids = (this.#db
      .prepare(`SELECT id FROM sessions WHERE archived_at IS NOT NULL AND archived_at < ?`)
      .all(cutoff) as { id: string }[]).map((r) => r.id);
    if (ids.length === 0) return 0;
    const ph = ids.map(() => "?").join(",");
    this.#db.exec("PRAGMA foreign_keys = OFF");
    try {
      for (
        const t of ["message_embeddings", "turns", "net_events", "net_policies"]
      ) {
        this.#db.prepare(`DELETE FROM ${t} WHERE session_id IN (${ph})`).run(...ids);
      }
      this.#db.prepare(`DELETE FROM messages WHERE session_id IN (${ph})`).run(...ids);
      // Survivors that pointed at a purged parent lose that pointer (surface as roots).
      this.#db.prepare(`UPDATE sessions SET parent_id = NULL WHERE parent_id IN (${ph})`).run(
        ...ids,
      );
      this.#db.prepare(`DELETE FROM sessions WHERE id IN (${ph})`).run(...ids);
    } finally {
      this.#db.exec("PRAGMA foreign_keys = ON");
    }
    return ids.length;
  }

  /** The root→self chain of sessions (root first). Empty if id is unknown. */
  ancestorChain(id: string): Session[] {
    const chain: Session[] = [];
    const seen = new Set<string>();
    let cur = this.getSession(id);
    while (cur && !seen.has(cur.id)) {
      seen.add(cur.id);
      chain.push(cur);
      cur = cur.parentId ? this.getSession(cur.parentId) : undefined;
    }
    chain.reverse();
    return chain;
  }

  // messages ----------------------------------------------------------------

  createMessage(m: Message): Message {
    this.#db
      .prepare(
        `INSERT INTO messages (id, session_id, role, parts, pending, created_at)
         VALUES (?, ?, ?, ?, ?, ?)`,
      )
      .run(m.id, m.sessionId, m.role, JSON.stringify(m.parts), m.pending ? 1 : 0, m.createdAt);
    return m;
  }

  messagesFor(sessionId: string): Message[] {
    const rows = this.#db
      // rowid breaks created_at ties by insertion order (same-ms user + supervisor msgs).
      .prepare(`SELECT * FROM messages WHERE session_id = ? ORDER BY created_at, rowid`)
      .all(sessionId) as MessageRow[];
    return rows.map(toMessage);
  }

  getMessage(id: string): Message | undefined {
    const r = this.#db.prepare(`SELECT * FROM messages WHERE id = ?`).get(id) as
      | MessageRow
      | undefined;
    return r && toMessage(r);
  }

  /** Overwrite a message's parts and pending flag (the turn runner streams into this). */
  updateMessage(id: string, parts: Part[], pending: boolean): void {
    this.#db
      .prepare(`UPDATE messages SET parts = ?, pending = ? WHERE id = ?`)
      .run(JSON.stringify(parts), pending ? 1 : 0, id);
  }

  /**
   * The thread for a session: messages of every ancestor (root first) followed by the
   * session's own, each session's messages in creation order. This is the tree-history
   * read the UI opens a session with.
   */
  threadFor(id: string): Message[] {
    return this.ancestorChain(id).flatMap((s) => this.messagesFor(s.id));
  }

  // message embeddings (recall search) ----------------------------------------

  /** Newest messages with no embedding row yet — the lazy indexer's work queue. */
  messagesToEmbed(limit: number): Message[] {
    const rows = this.#db
      .prepare(
        `SELECT m.* FROM messages m
           LEFT JOIN message_embeddings e ON e.message_id = m.id
         WHERE e.message_id IS NULL AND m.pending = 0
         ORDER BY m.created_at DESC LIMIT ?`,
      )
      .all(limit) as MessageRow[];
    return rows.map(toMessage);
  }

  /** Store a message's unit vector; null marks "nothing to embed — don't retry". */
  putEmbedding(messageId: string, sessionId: string, vector: Float32Array | null): void {
    this.#db
      .prepare(
        `INSERT OR REPLACE INTO message_embeddings (message_id, session_id, dim, vector)
         VALUES (?, ?, ?, ?)`,
      )
      .run(
        messageId,
        sessionId,
        vector?.length ?? 0,
        vector ? new Uint8Array(vector.buffer, vector.byteOffset, vector.byteLength) : null,
      );
  }

  /** Every stored vector (dim>0 rows only), for the in-process cosine scan. */
  allEmbeddings(): { messageId: string; sessionId: string; vector: Float32Array }[] {
    const rows = this.#db
      .prepare(`SELECT message_id, session_id, dim, vector FROM message_embeddings WHERE dim > 0`)
      .all() as { message_id: string; session_id: string; dim: number; vector: Uint8Array }[];
    return rows.map((r) => ({
      messageId: r.message_id,
      sessionId: r.session_id,
      vector: new Float32Array(r.vector.buffer, r.vector.byteOffset, r.dim),
    }));
  }

  // turns -------------------------------------------------------------------

  createTurn(t: Turn): Turn {
    this.#db
      .prepare(
        `INSERT INTO turns (id, session_id, message_id, status, step, updated_at)
         VALUES (?, ?, ?, ?, ?, ?)`,
      )
      .run(t.id, t.sessionId, t.messageId, t.status, t.step, t.updatedAt);
    return t;
  }

  /**
   * Stamp when the turn's first output reached the user (idempotent — only the
   * first call lands). Doesn't bump updated_at: this is a metric, not a checkpoint.
   */
  setTurnFirstOutput(id: string, ts: number): void {
    this.#db
      .prepare(`UPDATE turns SET first_output_at = ? WHERE id = ? AND first_output_at IS NULL`)
      .run(ts, id);
  }

  /** All turns for a session, oldest first (metrics.ts). */
  turnsForSession(sessionId: string): Turn[] {
    const rows = this.#db
      .prepare(`SELECT * FROM turns WHERE session_id = ? ORDER BY updated_at`)
      .all(sessionId) as TurnRow[];
    return rows.map(toTurn);
  }

  /** Patch a turn's status and/or step, bumping updated_at. Used for checkpoints. */
  updateTurn(id: string, patch: { status?: TurnStatus; step?: string }): void {
    const cur = this.getTurn(id);
    if (!cur) return;
    this.#db
      .prepare(`UPDATE turns SET status = ?, step = ?, updated_at = ? WHERE id = ?`)
      .run(patch.status ?? cur.status, patch.step ?? cur.step, Date.now(), id);
  }

  /**
   * Latest turn status per session (SQLite bare-column-with-MAX picks the row of
   * the max). Sessions that never ran a turn are absent. Feeds the sidebar/map
   * status affixes via GET /sessions.
   */
  latestTurnStatuses(): Map<string, TurnStatus> {
    const rows = this.#db
      .prepare(`SELECT session_id, status, MAX(updated_at) FROM turns GROUP BY session_id`)
      .all() as { session_id: string; status: string }[];
    return new Map(rows.map((r) => [r.session_id, r.status as TurnStatus]));
  }

  /** The turn that produced a given supervisor message (latest row wins). */
  turnForMessage(messageId: string): Turn | undefined {
    const r = this.#db
      .prepare(`SELECT * FROM turns WHERE message_id = ? ORDER BY updated_at DESC LIMIT 1`)
      .get(messageId) as TurnRow | undefined;
    return r && toTurn(r);
  }

  getTurn(id: string): Turn | undefined {
    const r = this.#db.prepare(`SELECT * FROM turns WHERE id = ?`).get(id) as
      | TurnRow
      | undefined;
    return r && toTurn(r);
  }

  turnsByStatus(status: TurnStatus): Turn[] {
    const rows = this.#db
      .prepare(`SELECT * FROM turns WHERE status = ? ORDER BY updated_at`)
      .all(status) as TurnRow[];
    return rows.map(toTurn);
  }

  // net_policies ------------------------------------------------------------

  /** The raw NetConfig JSON overriding the policy for this session, if any. */
  getNetPolicy(sessionId: string): string | undefined {
    const row = this.#db
      .prepare(`SELECT config FROM net_policies WHERE session_id = ?`)
      .get(sessionId) as { config: string } | undefined;
    return row?.config;
  }

  setNetPolicy(sessionId: string, config: string): void {
    this.#db
      .prepare(
        `INSERT OR REPLACE INTO net_policies (session_id, config, updated_at) VALUES (?, ?, ?)`,
      )
      .run(sessionId, config, Date.now());
  }

  deleteNetPolicy(sessionId: string): void {
    this.#db.prepare(`DELETE FROM net_policies WHERE session_id = ?`).run(sessionId);
  }

  // net_events --------------------------------------------------------------

  /**
   * Upsert (by id) one net-gate decision. A held request is written twice — first
   * `pending`, then resolved to allowed/denied — so INSERT OR REPLACE keeps the row
   * at its latest verdict, which is what the rail and approval card render.
   */
  recordNetEvent(sessionId: string | undefined, r: NetRequest): void {
    this.#db
      .prepare(
        `INSERT OR REPLACE INTO net_events
           (id, session_id, host, verb, action, verdict, reason, requested_by, fields, ts)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        r.id,
        sessionId ?? null,
        r.host,
        r.verb ?? null,
        r.action,
        r.verdict,
        r.reason ?? null,
        r.requestedBy ?? null,
        r.fields ? JSON.stringify(r.fields) : null,
        r.ts,
      );
  }

  /** Specific net-gate rows by id (order preserved); unknown ids are skipped. */
  netEventsByIds(ids: string[]): NetRequest[] {
    if (ids.length === 0) return [];
    const marks = ids.map(() => "?").join(",");
    const rows = this.#db
      .prepare(`SELECT * FROM net_events WHERE id IN (${marks})`)
      .all(...ids) as NetEventRow[];
    const byId = new Map(rows.map((r) => [r.id, toNetRequest(r)]));
    return ids.map((id) => byId.get(id)).filter((r): r is NetRequest => r !== undefined);
  }

  /**
   * Flip every still-`pending` net event to denied (boot sweep): no hold survives a
   * restart, so a pending row at startup is an orphan that would otherwise show an
   * unanswerable approval card forever. Returns how many were swept.
   */
  expirePendingNetEvents(reason: string): number {
    const out = this.#db
      .prepare(`UPDATE net_events SET verdict = 'denied', reason = ? WHERE verdict = 'pending'`)
      .run(reason);
    return Number(out.changes);
  }

  /** Recent net-gate decisions, newest first; filtered by session when given. */
  recentNetEvents(sessionId?: string, limit = 100): NetRequest[] {
    const rows = (sessionId
      ? this.#db
        .prepare(
          `SELECT * FROM net_events WHERE session_id = ? ORDER BY ts DESC, rowid DESC LIMIT ?`,
        )
        .all(sessionId, limit)
      : this.#db
        .prepare(`SELECT * FROM net_events ORDER BY ts DESC, rowid DESC LIMIT ?`)
        .all(limit)) as NetEventRow[];
    return rows.map(toNetRequest);
  }
}

type NetEventRow = {
  id: string;
  session_id: string | null;
  host: string;
  verb: string | null;
  action: string;
  verdict: string;
  reason: string | null;
  requested_by: string | null;
  fields: string | null;
  ts: number;
};

function toNetRequest(r: NetEventRow): NetRequest {
  const out: NetRequest = {
    id: r.id,
    host: r.host,
    action: r.action,
    verdict: r.verdict as NetRequest["verdict"],
    ts: r.ts,
  };
  if (r.session_id != null) out.sessionId = r.session_id;
  if (r.verb != null) out.verb = r.verb;
  if (r.reason != null) out.reason = r.reason;
  if (r.requested_by != null) out.requestedBy = r.requested_by;
  if (r.fields != null) {
    try {
      out.fields = JSON.parse(r.fields);
    } catch {
      // corrupt fields blob — drop it, the row still renders
    }
  }
  return out;
}

/** Resolve the DB path: BOUGH_DB override, else ~/.bough/bough.db (dir created). */
export function defaultDbPath(): string {
  const override = Deno.env.get("BOUGH_DB");
  if (override) return override;
  const dir = join(homedir(), ".bough");
  mkdirSync(dir, { recursive: true });
  return join(dir, "bough.db");
}

export function openDb(path: string = defaultDbPath()): Db {
  return new Db(path);
}
