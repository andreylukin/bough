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
  base        TEXT,                   -- persisted jj/git base commit, captured on the first turn
  origin_id         TEXT,             -- lineage: session this fork/compaction branched from (null for root/plain)
  origin_message_id TEXT,             -- lineage: the fork-at message / compaction span-end message
  archived_at INTEGER,                -- soft delete: archived sessions leave the sidebar, rows stay
  context_tokens INTEGER,             -- last turn's prompt size (context meter)
  output_tokens  INTEGER              -- cumulative output tokens across the session
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
  ts           INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS snapshots (
  id          TEXT PRIMARY KEY,
  session_id  TEXT NOT NULL REFERENCES sessions(id),
  ref         TEXT NOT NULL,          -- jj change id / clonefile path
  created_at  INTEGER NOT NULL
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
  updated_at  INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS turns_status ON turns(status);
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
};

/**
 * The runtime (non-wire) facts a session carries for the turn runner: its explicit
 * workspace (null = fall back to BOUGH_WORKSPACE/cwd) and its persisted jj/git base.
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
}
type TurnRow = {
  id: string;
  session_id: string;
  message_id: string;
  status: string;
  step: string;
  updated_at: number;
};

function toTurn(r: TurnRow): Turn {
  return {
    id: r.id,
    sessionId: r.session_id,
    messageId: r.message_id,
    status: r.status as TurnStatus,
    step: r.step,
    updatedAt: r.updated_at,
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
        "context_tokens INTEGER",
        "output_tokens INTEGER",
      ]
    ) {
      try {
        this.#db.exec(`ALTER TABLE sessions ADD COLUMN ${col}`);
      } catch {
        // column already exists
      }
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

  /** Token usage for the context meter: last turn's prompt size + cumulative output. */
  setSessionUsage(id: string, contextTokens: number, outputTokens: number): void {
    this.#db
      .prepare(`UPDATE sessions SET context_tokens = ?, output_tokens = ? WHERE id = ?`)
      .run(contextTokens, outputTokens, id);
  }

  sessionUsage(id: string): { contextTokens: number; outputTokens: number } {
    const r = this.#db
      .prepare(`SELECT context_tokens, output_tokens FROM sessions WHERE id = ?`)
      .get(id) as { context_tokens: number | null; output_tokens: number | null } | undefined;
    return { contextTokens: r?.context_tokens ?? 0, outputTokens: r?.output_tokens ?? 0 };
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

  /** Patch a turn's status and/or step, bumping updated_at. Used for checkpoints. */
  updateTurn(id: string, patch: { status?: TurnStatus; step?: string }): void {
    const cur = this.getTurn(id);
    if (!cur) return;
    this.#db
      .prepare(`UPDATE turns SET status = ?, step = ?, updated_at = ? WHERE id = ?`)
      .run(patch.status ?? cur.status, patch.step ?? cur.step, Date.now(), id);
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

  // net_events --------------------------------------------------------------

  recordNetEvent(sessionId: string | undefined, r: NetRequest): void {
    this.#db
      .prepare(
        `INSERT INTO net_events (id, session_id, host, verb, action, verdict, reason, requested_by, ts)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
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
        r.ts,
      );
  }
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
