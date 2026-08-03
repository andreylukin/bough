/**
 * The only place in the tree that speaks SQL.
 *
 * The invariant: **no raw SQL exists outside `db/`.** Every read and write in the
 * system goes through a typed method here, which is what makes the ordering rules
 * below enforceable at all — they are properties of three `ORDER BY` clauses in one
 * file, not a convention every caller has to remember.
 *
 * The three ordering rules, in order of how much depends on them:
 *
 *   1. `messagesFor` orders by `(created_at, rowid)`, never `created_at` alone.
 *      Branch seeding writes with a real clock rather than an advanced artificial
 *      one (plan §6.1), so a turn started immediately after a seed lands in the
 *      *same millisecond* — `rowid`, the insertion order, is the only thing that
 *      keeps it after the seed. Sorting by timestamp alone reorders history under
 *      the user.
 *   2. `threadFor` is every ancestor's messages root→parent, then the session's own.
 *      This is what makes fork and compaction cheap: a branch parented at the
 *      target's parent inherits the shared ancestors for free and seeds only the
 *      rest (spec §14).
 *   3. `ancestorChain` walks `parent_id` to the lineage root and returns it root
 *      first, inclusive of the session itself. `session_state` is scoped to
 *      `chain[0]`, so a fork and its parent read one store (spec §6).
 *
 * What this layer is NOT: a place for policy. `listSessions` returns every session
 * and the *caller* derives visibility from `kind` + `origin_id` — there is no
 * archive, deprecate or purge column to filter on, because there is no such action
 * (spec §4, §17). Likewise there are no embeddings: cross-session search is keyword
 * FTS over a text projection of `parts`.
 *
 * Injection: the database path and the clock are constructor arguments. `updateTurn`
 * is the one method that stamps a time of its own, and it stamps `#now()` — so a
 * test drives checkpoint ordering without sleeping. Everything else takes its
 * timestamps from the caller.
 *
 * Storage conventions, matching `schema.sql`: timestamps are epoch ms integers,
 * booleans are 0/1, and anything structured is JSON text.
 */
import { Database } from "bun:sqlite";
import { mkdirSync } from "node:fs";
import { dirname } from "node:path";
import { dbPath } from "../paths.ts";
import { BadRequestError } from "../errors.ts";
import { migrate } from "./migrate.ts";
import type {
  CommandRecord,
  CommandTagRow,
  Db as DbPort,
  SearchHit,
  SessionRuntime,
  TagDiversityDay,
  TaggedCommand,
  UsageTotals,
} from "../types.ts";
import type {
  Message,
  Part,
  Role,
  Schedule,
  Session,
  SessionKind,
  Turn,
  TurnStatus,
  Usage,
  WorkflowAgent,
  WorkflowAgentStatus,
  WorkflowPhase,
  WorkflowRun,
  WorkflowStatus,
} from "../schema/parts.ts";

// ---- rows -------------------------------------------------------------------
// One type per table, named exactly as the columns are. The mappers below are the
// only translation between snake_case storage and the camelCase wire shapes.

type SessionRow = {
  id: string;
  parent_id: string | null;
  title: string;
  kind: string;
  created_at: number;
  workspace: string | null;
  origin_dir: string | null;
  base: string | null;
  origin_id: string | null;
  origin_message_id: string | null;
  model: string | null;
  effort: string | null;
  draft: string | null;
  context_tokens: number | null;
  cached_tokens: number | null;
  last_llm_at: number | null;
  input_tokens: number | null;
  output_tokens: number | null;
  reasoning_tokens: number | null;
  cache_read_total: number | null;
  cache_write_total: number | null;
  cost_usd: number | null;
  outcome_ok: number | null;
};

type MessageRow = {
  id: string;
  session_id: string;
  role: string;
  parts: string;
  pending: number;
  created_at: number;
};

type TurnRow = {
  id: string;
  session_id: string;
  message_id: string;
  status: string;
  step: string;
  created_at: number;
  updated_at: number;
  error: string | null;
  input_tokens: number | null;
  output_tokens: number | null;
  reasoning_tokens: number | null;
  cache_read_tokens: number | null;
  cache_write_tokens: number | null;
  cost_usd: number | null;
};

type ScheduleRow = {
  id: string;
  title: string;
  prompt: string;
  workspace: string | null;
  spec: string;
  enabled: number;
  created_at: number;
  last_run_at: number | null;
  next_run_at: number;
  session_id: string | null;
};

type WorkflowRow = {
  id: string;
  session_id: string;
  name: string;
  description: string;
  script: string;
  phases: string;
  status: string;
  current_phase: string | null;
  result: string | null;
  error: string | null;
  args: string | null;
  resume_of: string | null;
  created_at: number;
  finished_at: number | null;
};

type WorkflowAgentRow = {
  id: string;
  run_id: string;
  idx: number;
  key: string;
  label: string;
  phase: string | null;
  prompt: string;
  model: string | null;
  schema: string | null;
  status: string;
  result: string | null;
  error: string | null;
  session_id: string | null;
  started_at: number;
  finished_at: number | null;
};

// ---- row → domain -----------------------------------------------------------

/** Absent optionals come back as `null`, never `undefined`: one shape per row. */
function toSession(r: SessionRow): Session {
  return {
    id: r.id,
    parentId: r.parent_id,
    title: r.title,
    kind: r.kind as SessionKind,
    createdAt: r.created_at,
    workspace: r.workspace,
    originDir: r.origin_dir,
    base: r.base,
    originId: r.origin_id,
    originMessageId: r.origin_message_id,
    model: r.model,
    effort: r.effort,
    draft: r.draft,
    contextTokens: r.context_tokens,
    cachedTokens: r.cached_tokens,
    lastLlmAt: r.last_llm_at,
    outcomeOk: r.outcome_ok === null ? null : r.outcome_ok === 1,
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

/**
 * A turn's `usage` is present once the provider has reported anything for it — a
 * turn that errored before its first round has none, and reporting zeros there
 * would be a claim we cannot make.
 */
function toTurn(r: TurnRow): Turn {
  const reported = r.input_tokens !== null || r.output_tokens !== null;
  return {
    id: r.id,
    sessionId: r.session_id,
    messageId: r.message_id,
    status: r.status as TurnStatus,
    step: r.step,
    createdAt: r.created_at,
    updatedAt: r.updated_at,
    error: r.error,
    usage: reported
      ? {
        inputTokens: r.input_tokens ?? 0,
        outputTokens: r.output_tokens ?? 0,
        reasoningTokens: r.reasoning_tokens,
        cacheReadTokens: r.cache_read_tokens,
        cacheWriteTokens: r.cache_write_tokens,
        costUsd: r.cost_usd,
      }
      : null,
  };
}

function toSchedule(r: ScheduleRow): Schedule {
  return {
    id: r.id,
    title: r.title,
    prompt: r.prompt,
    workspace: r.workspace,
    spec: r.spec,
    enabled: r.enabled === 1,
    createdAt: r.created_at,
    lastRunAt: r.last_run_at,
    nextRunAt: r.next_run_at,
    sessionId: r.session_id,
  };
}

function toWorkflow(r: WorkflowRow): WorkflowRun {
  return {
    id: r.id,
    sessionId: r.session_id,
    name: r.name,
    description: r.description,
    script: r.script,
    phases: JSON.parse(r.phases) as WorkflowPhase[],
    status: r.status as WorkflowStatus,
    currentPhase: r.current_phase,
    result: r.result === null ? null : JSON.parse(r.result),
    error: r.error,
    args: r.args === null ? null : JSON.parse(r.args),
    resumeOf: r.resume_of,
    createdAt: r.created_at,
    finishedAt: r.finished_at,
  };
}

function toWorkflowAgent(r: WorkflowAgentRow): WorkflowAgent {
  return {
    id: r.id,
    runId: r.run_id,
    idx: r.idx,
    key: r.key,
    label: r.label,
    phase: r.phase,
    prompt: r.prompt,
    model: r.model,
    status: r.status as WorkflowAgentStatus,
    result: r.result,
    error: r.error,
    sessionId: r.session_id,
    startedAt: r.started_at,
    finishedAt: r.finished_at,
  };
}

// ---- small helpers ----------------------------------------------------------

const bit = (v: boolean): number => (v ? 1 : 0);

/** `undefined` is not a bindable value; nullish optionals all store as NULL. */
const nul = <T>(v: T | null | undefined): T | null => (v === undefined ? null : v);

/** JSON columns: `undefined` and `null` both store as NULL, so a read round-trips. */
const json = (v: unknown): string | null =>
  v === undefined || v === null ? null : JSON.stringify(v);

/**
 * The text a message contributes to the keyword index: its prose and its
 * reasoning. Tool calls, results and image paths are deliberately excluded — a
 * search over transcripts should find what was *said*, not a path that happened to
 * appear in a directory listing (spec §17: FTS over transcripts).
 */
function indexableText(parts: Part[]): string {
  return parts
    .filter((p): p is Extract<Part, { type: "text" | "reasoning" }> =>
      p.type === "text" || p.type === "reasoning"
    )
    .map((p) => p.text)
    .join("\n")
    .trim();
}

/** How `openDb`/`SqliteDb` take their seams. */
export interface DbOptions {
  /** Injected clock. Absent = `Date.now`. Only `updateTurn` reads it. */
  now?: () => number;
}

// ---- the handle -------------------------------------------------------------

/**
 * The concrete `Db`. Satisfies the port in `types.ts`; consumers that only need
 * the port depend on that file, not on this one.
 */
export class SqliteDb implements DbPort {
  #db: Database;
  #now: () => number;

  constructor(path: string, opts: DbOptions = {}) {
    this.#db = new Database(path);
    this.#now = opts.now ?? Date.now;
    // Declared foreign keys are only enforced when this is on, and it is a
    // per-connection setting — off by default, so it must be set at every open.
    this.#db.exec("PRAGMA foreign_keys = ON");
    migrate(this.#db);
  }

  close(): void {
    this.#db.close();
  }

  #all<T>(sql: string, ...params: (string | number | null)[]): T[] {
    return this.#db.prepare(sql).all(...params) as unknown as T[];
  }

  /**
   * `bun:sqlite` reports "no row" as `null`. Absence is `undefined` everywhere above
   * this line — `getSchedule(unknown)` returns `undefined`, not `null` — so the one
   * place that knows about the driver is the one place that normalises it.
   */
  #get<T>(sql: string, ...params: (string | number | null)[]): T | undefined {
    return (this.#db.prepare(sql).get(...params) as unknown as T | null) ?? undefined;
  }

  #run(sql: string, ...params: (string | number | null)[]): void {
    this.#db.prepare(sql).run(...params);
  }

  // ---- sessions -------------------------------------------------------------

  /**
   * Insert and return the row *as stored*. Reading back rather than echoing the
   * argument is deliberate: `createSession(s)` and `getSession(s.id)` then agree
   * field for field, so a caller can never be handed a session carrying a value the
   * database did not keep.
   */
  createSession(s: Session): Session {
    this.#run(
      `INSERT INTO sessions
         (id, parent_id, title, kind, created_at, workspace, origin_dir, base,
          origin_id, origin_message_id, model, effort, draft)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      s.id,
      s.parentId,
      s.title,
      s.kind,
      s.createdAt,
      nul(s.workspace),
      nul(s.originDir),
      nul(s.base),
      nul(s.originId),
      nul(s.originMessageId),
      nul(s.model),
      nul(s.effort),
      nul(s.draft),
    );
    return this.getSession(s.id)!;
  }

  getSession(id: string): Session | undefined {
    const r = this.#get<SessionRow>(`SELECT * FROM sessions WHERE id = ?`, id);
    return r && toSession(r);
  }

  getSessionRuntime(id: string): SessionRuntime {
    const r = this.#get<{ workspace: string | null; base: string | null }>(
      `SELECT workspace, base FROM sessions WHERE id = ?`,
      id,
    );
    return { workspace: r?.workspace ?? null, base: r?.base ?? null };
  }

  /**
   * Every session, newest first. No visibility filter: `subagent` and
   * `workflow_agent` rows are excluded by the *caller*, which derives that from
   * `kind` + `originId` (spec §4). Tie-broken by `rowid` so two sessions created in
   * one millisecond have a stable order.
   */
  listSessions(): Session[] {
    return this.#all<SessionRow>(
      `SELECT * FROM sessions ORDER BY created_at DESC, rowid DESC`,
    ).map(toSession);
  }

  /** The branches collapsed under `originId`, in creation order — the drill-in. */
  sessionsByOrigin(originId: string): Session[] {
    return this.#all<SessionRow>(
      `SELECT * FROM sessions WHERE origin_id = ? ORDER BY created_at, rowid`,
      originId,
    ).map(toSession);
  }

  /**
   * Root→self, inclusive; `[]` for an unknown id. The `seen` set is not paranoia
   * about a well-formed tree — it is what stops a cycle introduced by a bad write
   * from hanging the server on every read of that session.
   */
  ancestorChain(id: string): Session[] {
    const chain: Session[] = [];
    const seen = new Set<string>();
    let cur = this.getSession(id);
    while (cur && !seen.has(cur.id)) {
      seen.add(cur.id);
      chain.push(cur);
      cur = cur.parentId ? this.getSession(cur.parentId) : undefined;
    }
    return chain.reverse();
  }

  setSessionTitle(id: string, title: string): void {
    this.#run(`UPDATE sessions SET title = ? WHERE id = ?`, title, id);
  }

  setSessionWorkspace(id: string, workspace: string): void {
    this.#run(`UPDATE sessions SET workspace = ? WHERE id = ?`, workspace, id);
  }

  setSessionBase(id: string, base: string): void {
    this.#run(`UPDATE sessions SET base = ? WHERE id = ?`, base, id);
  }

  /** Set by handoff; cleared with `null` by the first posted message. */
  setSessionDraft(id: string, draft: string | null): void {
    this.#run(`UPDATE sessions SET draft = ? WHERE id = ?`, draft, id);
  }

  /** `null` clears the pin back to the global default (spec §12). */
  setSessionModel(id: string, model: string | null): void {
    this.#run(`UPDATE sessions SET model = ? WHERE id = ?`, model, id);
  }

  setSessionEffort(id: string, effort: string | null): void {
    this.#run(`UPDATE sessions SET effort = ? WHERE id = ?`, effort, id);
  }

  /** Whether the delegated TURN errored. Not an acceptance gate (spec §17). */
  setSessionOutcome(id: string, ok: boolean): void {
    this.#run(`UPDATE sessions SET outcome_ok = ? WHERE id = ?`, bit(ok), id);
  }

  /**
   * Fold one round's usage into the session.
   *
   * Two different things happen here and conflating them is the classic bug: the
   * cost columns ACCUMULATE across the session, while `context_tokens` /
   * `cached_tokens` / `last_llm_at` are OVERWRITTEN — they describe the last round
   * only, because the context meter is a gauge, not a total.
   *
   * `context_tokens` is the whole prompt the provider saw, which is the uncached
   * input plus everything read from or written to the cache; `cached_tokens` is the
   * share of that which was cached. The client derives cache warmth from
   * `last_llm_at` and a TTL rather than reading a stored boolean.
   */
  addSessionUsage(id: string, usage: Usage, at: number): void {
    const read = usage.cacheReadTokens ?? 0;
    const write = usage.cacheWriteTokens ?? 0;
    this.#run(
      `UPDATE sessions SET
         input_tokens      = COALESCE(input_tokens, 0) + ?,
         output_tokens     = COALESCE(output_tokens, 0) + ?,
         reasoning_tokens  = COALESCE(reasoning_tokens, 0) + ?,
         cache_read_total  = COALESCE(cache_read_total, 0) + ?,
         cache_write_total = COALESCE(cache_write_total, 0) + ?,
         cost_usd          = COALESCE(cost_usd, 0) + ?,
         context_tokens    = ?,
         cached_tokens     = ?,
         last_llm_at       = ?
       WHERE id = ?`,
      usage.inputTokens,
      usage.outputTokens,
      usage.reasoningTokens ?? 0,
      read,
      write,
      usage.costUsd ?? 0,
      usage.inputTokens + read + write,
      read + write,
      at,
      id,
    );
  }

  sessionUsage(id: string): UsageTotals {
    const r = this.#get<SessionRow>(`SELECT * FROM sessions WHERE id = ?`, id);
    return {
      inputTokens: r?.input_tokens ?? 0,
      outputTokens: r?.output_tokens ?? 0,
      reasoningTokens: r?.reasoning_tokens ?? 0,
      cacheReadTokens: r?.cache_read_total ?? 0,
      cacheWriteTokens: r?.cache_write_total ?? 0,
      costUsd: r?.cost_usd ?? 0,
    };
  }

  /**
   * The session plus every branch that collapsed under it, transitively — what the
   * tree view shows as one piece of work's cost.
   *
   * Follows `origin_id` but only through `subagent` / `workflow_agent` rows: a fork
   * or a compaction is a sibling the user opened deliberately, and charging its
   * spend to the session it branched from would double-count the tree total.
   * `UNION` (not `UNION ALL`) so a cyclic `origin_id` terminates instead of looping.
   */
  treeUsage(id: string): UsageTotals {
    const r = this.#get<{
      input: number;
      output: number;
      reasoning: number;
      read: number;
      write: number;
      cost: number;
    }>(
      `WITH RECURSIVE tree(id) AS (
         SELECT id FROM sessions WHERE id = ?
         UNION
         SELECT s.id FROM sessions s JOIN tree t ON s.origin_id = t.id
          WHERE s.kind IN ('subagent', 'workflow_agent')
       )
       SELECT COALESCE(SUM(input_tokens), 0)      AS input,
              COALESCE(SUM(output_tokens), 0)     AS output,
              COALESCE(SUM(reasoning_tokens), 0)  AS reasoning,
              COALESCE(SUM(cache_read_total), 0)  AS read,
              COALESCE(SUM(cache_write_total), 0) AS write,
              COALESCE(SUM(cost_usd), 0)          AS cost
         FROM sessions WHERE id IN (SELECT id FROM tree)`,
      id,
    )!;
    return {
      inputTokens: r.input,
      outputTokens: r.output,
      reasoningTokens: r.reasoning,
      cacheReadTokens: r.read,
      cacheWriteTokens: r.write,
      costUsd: r.cost,
    };
  }

  /**
   * Sessions with a turn in flight. Read from `turns`, not from a pending message:
   * an orphaned turn's message can still be pending after a restart, and a session
   * that looks busy forever is exactly the hang T2.3 recovery exists to prevent.
   */
  busySessionIds(): Set<string> {
    const rows = this.#all<{ session_id: string }>(
      `SELECT DISTINCT session_id FROM turns WHERE status = 'running'`,
    );
    return new Set(rows.map((r) => r.session_id));
  }

  // ---- messages -------------------------------------------------------------

  createMessage(m: Message): Message {
    this.#run(
      `INSERT INTO messages (id, session_id, role, parts, pending, created_at)
       VALUES (?, ?, ?, ?, ?, ?)`,
      m.id,
      m.sessionId,
      m.role,
      JSON.stringify(m.parts),
      bit(m.pending),
      m.createdAt,
    );
    return this.getMessage(m.id)!;
  }

  getMessage(id: string): Message | undefined {
    const r = this.#get<MessageRow>(`SELECT * FROM messages WHERE id = ?`, id);
    return r && toMessage(r);
  }

  /**
   * The session's own messages, oldest first.
   *
   * `ORDER BY created_at, rowid` — never `created_at` alone. Seeded branch messages
   * are written with a real clock (plan §6.1), so a turn started right afterwards
   * shares their millisecond; `rowid` is the insertion order that keeps it after the
   * seed. Dropping the tie-break silently reorders history.
   */
  messagesFor(sessionId: string): Message[] {
    return this.#all<MessageRow>(
      `SELECT * FROM messages WHERE session_id = ? ORDER BY created_at, rowid`,
      sessionId,
    ).map(toMessage);
  }

  /**
   * The full replayable thread: every ancestor's messages root→parent, then the
   * session's own. A fork parented at its target's parent therefore inherits the
   * shared history without copying a byte of it (spec §14).
   */
  threadFor(sessionId: string): Message[] {
    return this.ancestorChain(sessionId).flatMap((s) => this.messagesFor(s.id));
  }

  /** Wholesale overwrite — the turn runner streams into this every round. */
  updateMessage(id: string, parts: Part[], pending: boolean): void {
    this.#run(
      `UPDATE messages SET parts = ?, pending = ? WHERE id = ?`,
      JSON.stringify(parts),
      bit(pending),
      id,
    );
  }

  // ---- turns ----------------------------------------------------------------

  createTurn(t: Turn): Turn {
    this.#run(
      `INSERT INTO turns (id, session_id, message_id, status, step, created_at, updated_at, error)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?)`,
      t.id,
      t.sessionId,
      t.messageId,
      t.status,
      t.step,
      t.createdAt,
      t.updatedAt,
      nul(t.error),
    );
    return this.getTurn(t.id)!;
  }

  getTurn(id: string): Turn | undefined {
    const r = this.#get<TurnRow>(`SELECT * FROM turns WHERE id = ?`, id);
    return r && toTurn(r);
  }

  /** The turn that produced a supervisor message; the most recently touched wins. */
  turnForMessage(messageId: string): Turn | undefined {
    const r = this.#get<TurnRow>(
      `SELECT * FROM turns WHERE message_id = ? ORDER BY updated_at DESC, rowid DESC LIMIT 1`,
      messageId,
    );
    return r && toTurn(r);
  }

  turnsForSession(sessionId: string): Turn[] {
    return this.#all<TurnRow>(
      `SELECT * FROM turns WHERE session_id = ? ORDER BY created_at, rowid`,
      sessionId,
    ).map(toTurn);
  }

  /** Boot recovery reads `running` here and orphans every row it finds (T2.3). */
  turnsByStatus(status: TurnStatus): Turn[] {
    return this.#all<TurnRow>(
      `SELECT * FROM turns WHERE status = ? ORDER BY created_at, rowid`,
      status,
    ).map(toTurn);
  }

  /**
   * The latest turn status per session, for the sidebar affixes. Correlated
   * `LIMIT 1` rather than `GROUP BY` with a bare column: bare-column-with-MAX picks
   * an arbitrary row among ties, and two checkpoints in one millisecond is the
   * normal case, not the rare one.
   */
  latestTurnStatuses(): Map<string, TurnStatus> {
    const rows = this.#all<{ session_id: string; status: string }>(
      `SELECT session_id, status FROM turns
        WHERE rowid = (
          SELECT rowid FROM turns x WHERE x.session_id = turns.session_id
           ORDER BY x.updated_at DESC, x.rowid DESC LIMIT 1
        )`,
    );
    return new Map(rows.map((r) => [r.session_id, r.status as TurnStatus]));
  }

  /**
   * Checkpoint a turn. Every call bumps `updated_at` from the injected clock — the
   * point of a checkpoint is that it says *when* the turn last made progress.
   *
   * `usage` REPLACES the turn's usage columns rather than accumulating: the runner
   * carries the turn's running total and checkpoints it, so adding here would
   * double-count every round after the first. Session totals accumulate; that is
   * `addSessionUsage`.
   */
  updateTurn(
    id: string,
    patch: { status?: TurnStatus; step?: string; error?: string | null; usage?: Usage },
  ): void {
    const cur = this.#get<TurnRow>(`SELECT * FROM turns WHERE id = ?`, id);
    if (!cur) return;
    const u = patch.usage;
    this.#run(
      `UPDATE turns SET status = ?, step = ?, updated_at = ?, error = ?,
         input_tokens = ?, output_tokens = ?, reasoning_tokens = ?,
         cache_read_tokens = ?, cache_write_tokens = ?, cost_usd = ?
       WHERE id = ?`,
      patch.status ?? cur.status,
      patch.step ?? cur.step,
      this.#now(),
      "error" in patch ? nul(patch.error) : cur.error,
      u ? u.inputTokens : cur.input_tokens,
      u ? u.outputTokens : cur.output_tokens,
      u ? nul(u.reasoningTokens) : cur.reasoning_tokens,
      u ? nul(u.cacheReadTokens) : cur.cache_read_tokens,
      u ? nul(u.cacheWriteTokens) : cur.cache_write_tokens,
      u ? nul(u.costUsd) : cur.cost_usd,
      id,
    );
  }

  // ---- durable KV, scoped to the lineage root --------------------------------

  getState(rootId: string, key: string): string | undefined {
    return this.#get<{ value: string }>(
      `SELECT value FROM session_state WHERE root_id = ? AND key = ?`,
      rootId,
      key,
    )?.value;
  }

  /** Upsert: a re-set overwrites in place and re-stamps `updated_at`. */
  setState(rootId: string, key: string, value: string, now: number): void {
    this.#run(
      `INSERT INTO session_state (root_id, key, value, updated_at) VALUES (?, ?, ?, ?)
       ON CONFLICT(root_id, key)
         DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at`,
      rootId,
      key,
      value,
      now,
    );
  }

  /** Keys and sizes only — a listing must never drag whole values into context. */
  listState(rootId: string): { key: string; bytes: number; updatedAt: number }[] {
    return this.#all<{ key: string; bytes: number; updated_at: number }>(
      `SELECT key, length(value) AS bytes, updated_at FROM session_state
        WHERE root_id = ? ORDER BY key`,
      rootId,
    ).map((r) => ({ key: r.key, bytes: r.bytes, updatedAt: r.updated_at }));
  }

  /** True when a row was actually removed, so the program learns "there was none". */
  deleteState(rootId: string, key: string): boolean {
    const existed = this.getState(rootId, key) !== undefined;
    this.#run(`DELETE FROM session_state WHERE root_id = ? AND key = ?`, rootId, key);
    return existed;
  }

  // ---- schedules ------------------------------------------------------------

  createSchedule(s: Schedule): Schedule {
    this.#run(
      `INSERT INTO schedules
         (id, title, prompt, workspace, spec, enabled, created_at, last_run_at, next_run_at,
          session_id)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      s.id,
      s.title,
      s.prompt,
      s.workspace,
      s.spec,
      bit(s.enabled),
      s.createdAt,
      s.lastRunAt,
      s.nextRunAt,
      s.sessionId,
    );
    return this.getSchedule(s.id)!;
  }

  getSchedule(id: string): Schedule | undefined {
    const r = this.#get<ScheduleRow>(`SELECT * FROM schedules WHERE id = ?`, id);
    return r && toSchedule(r);
  }

  listSchedules(): Schedule[] {
    return this.#all<ScheduleRow>(
      `SELECT * FROM schedules ORDER BY created_at, rowid`,
    ).map(toSchedule);
  }

  /** The ticker's due set: enabled and past due, soonest first. */
  dueSchedules(now: number): Schedule[] {
    return this.#all<ScheduleRow>(
      `SELECT * FROM schedules WHERE enabled = 1 AND next_run_at <= ?
        ORDER BY next_run_at, rowid`,
      now,
    ).map(toSchedule);
  }

  /** Overwrites the mutable fields; the caller merges a PATCH into the full row. */
  updateSchedule(s: Schedule): void {
    this.#run(
      `UPDATE schedules SET title = ?, prompt = ?, workspace = ?, spec = ?, enabled = ?,
         last_run_at = ?, next_run_at = ? WHERE id = ?`,
      s.title,
      s.prompt,
      s.workspace,
      s.spec,
      bit(s.enabled),
      s.lastRunAt,
      s.nextRunAt,
      s.id,
    );
  }

  /**
   * Stamp a fire. The caller computes `nextRunAt` FROM NOW, never from the stale
   * stored value — a server down through N slots fires once and resumes cadence
   * rather than bursting N make-up runs (plan §6.8).
   */
  markScheduleRun(id: string, lastRunAt: number, nextRunAt: number): void {
    this.#run(
      `UPDATE schedules SET last_run_at = ?, next_run_at = ? WHERE id = ?`,
      lastRunAt,
      nextRunAt,
      id,
    );
  }

  deleteSchedule(id: string): void {
    this.#run(`DELETE FROM schedules WHERE id = ?`, id);
  }

  // ---- workflows ------------------------------------------------------------

  createWorkflow(w: WorkflowRun): WorkflowRun {
    this.#run(
      `INSERT INTO workflows
         (id, session_id, name, description, script, phases, status, current_phase,
          result, error, args, resume_of, created_at, finished_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      w.id,
      w.sessionId,
      w.name,
      w.description,
      w.script,
      JSON.stringify(w.phases),
      w.status,
      w.currentPhase,
      json(w.result),
      w.error,
      json(w.args),
      w.resumeOf,
      w.createdAt,
      w.finishedAt,
    );
    return this.getWorkflow(w.id)!;
  }

  getWorkflow(id: string): WorkflowRun | undefined {
    const r = this.#get<WorkflowRow>(`SELECT * FROM workflows WHERE id = ?`, id);
    return r && toWorkflow(r);
  }

  /**
   * Runs belonging to a session — meaning its whole LINEAGE's runs, not the ones
   * whose `session_id` matches exactly.
   *
   * A branch shows its source's transcript by two different mechanisms and needs both
   * covered. Shared ancestors are inherited by reference (`threadFor` walks
   * `parent_id`); the forked turns themselves are COPIED into the new session
   * (`history/branch.ts`), and the edge back to where they came from is `origin_id`.
   * Either way the inherited messages carry the source's `workflow` parts, so scoping
   * this to one id left every one of those cards with no run row to read: a fork of a
   * finished 4/4 fan-out rendered `⧉ name · launched`, with no status, no agent counts
   * and no elapsed time.
   *
   * `origin_id` is followed only for fork/compaction. On a subagent or a workflow
   * agent the same column means the SPAWNER, and a delegate listing its spawner's
   * runs would be showing work that is not its own.
   */
  listWorkflows(sessionId?: string): WorkflowRun[] {
    if (sessionId === undefined) {
      return this.#all<WorkflowRow>(`SELECT * FROM workflows ORDER BY created_at DESC, rowid DESC`)
        .map(toWorkflow);
    }
    const ids: string[] = [];
    const seen = new Set<string>();
    const queue = [sessionId];
    while (queue.length > 0) {
      const id = queue.shift()!;
      if (seen.has(id)) continue;
      seen.add(id);
      const s = this.getSession(id);
      if (!s) continue;
      ids.push(id);
      if (s.parentId) queue.push(s.parentId);
      if (s.originId && (s.kind === "fork" || s.kind === "compaction")) queue.push(s.originId);
    }
    if (ids.length === 0) return [];
    const rows = this.#all<WorkflowRow>(
      `SELECT * FROM workflows WHERE session_id IN (${ids.map(() => "?").join(", ")})
       ORDER BY created_at DESC, rowid DESC`,
      ...ids,
    );
    return rows.map(toWorkflow);
  }

  /** Runs still `running`/`paused` at boot — orphaned like turns. */
  unfinishedWorkflows(): WorkflowRun[] {
    return this.#all<WorkflowRow>(
      `SELECT * FROM workflows WHERE status IN ('running', 'paused') ORDER BY created_at, rowid`,
    ).map(toWorkflow);
  }

  /**
   * Patch a run's mutable fields. Membership (`"x" in patch`), not `!== undefined`,
   * so `{result: undefined}` and `{}` stay distinguishable — `result` and `args` are
   * `unknown` and `undefined` is a value a script can legitimately return.
   *
   * Identity fields (`id`, `sessionId`, `script`, `createdAt`) are not patchable:
   * the script text is the record of what actually ran, and a rerun is a NEW run
   * that points back via `resumeOf` rather than an edit of the old one.
   */
  updateWorkflow(id: string, patch: Partial<WorkflowRun>): void {
    const cur = this.getWorkflow(id);
    if (!cur) return;
    this.#run(
      `UPDATE workflows SET name = ?, description = ?, phases = ?, status = ?,
         current_phase = ?, result = ?, error = ?, args = ?, finished_at = ?
       WHERE id = ?`,
      "name" in patch ? patch.name! : cur.name,
      "description" in patch ? patch.description! : cur.description,
      JSON.stringify("phases" in patch ? patch.phases! : cur.phases),
      "status" in patch ? patch.status! : cur.status,
      "currentPhase" in patch ? nul(patch.currentPhase) : cur.currentPhase,
      json("result" in patch ? patch.result : cur.result),
      "error" in patch ? nul(patch.error) : cur.error,
      json("args" in patch ? patch.args : cur.args),
      "finishedAt" in patch ? nul(patch.finishedAt) : cur.finishedAt,
      id,
    );
  }

  createWorkflowAgent(a: WorkflowAgent): WorkflowAgent {
    this.#run(
      `INSERT INTO workflow_agents
         (id, run_id, idx, key, label, phase, prompt, model, schema, status, result,
          error, session_id, started_at, finished_at)
       VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      a.id,
      a.runId,
      a.idx,
      a.key,
      a.label,
      a.phase,
      a.prompt,
      a.model,
      // The `schema` column has no field on the wire `WorkflowAgent` (schema/parts.ts)
      // — the JSON Schema of a {schema} call is part of what `key` hashes, so a rerun
      // already re-runs the right calls without reading it back. See notes on T5.4.
      null,
      a.status,
      a.result,
      a.error,
      a.sessionId,
      a.startedAt,
      a.finishedAt,
    );
    return this.#agent(a.id)!;
  }

  #agent(id: string): WorkflowAgent | undefined {
    const r = this.#get<WorkflowAgentRow>(`SELECT * FROM workflow_agents WHERE id = ?`, id);
    return r && toWorkflowAgent(r);
  }

  /**
   * Patch a journal row. `startedAt` is patchable on purpose: a queued agent's clock
   * is reset when it actually starts, so elapsed time excludes time spent parked on
   * the run's semaphore — otherwise a saturated run shows N agents "working" when
   * only `concurrency()` of them are.
   */
  updateWorkflowAgent(id: string, patch: Partial<WorkflowAgent>): void {
    const cur = this.#agent(id);
    if (!cur) return;
    this.#run(
      `UPDATE workflow_agents SET label = ?, phase = ?, status = ?, result = ?, error = ?,
         session_id = ?, started_at = ?, finished_at = ? WHERE id = ?`,
      "label" in patch ? patch.label! : cur.label,
      "phase" in patch ? nul(patch.phase) : cur.phase,
      "status" in patch ? patch.status! : cur.status,
      "result" in patch ? nul(patch.result) : cur.result,
      "error" in patch ? nul(patch.error) : cur.error,
      "sessionId" in patch ? nul(patch.sessionId) : cur.sessionId,
      "startedAt" in patch ? patch.startedAt! : cur.startedAt,
      "finishedAt" in patch ? nul(patch.finishedAt) : cur.finishedAt,
      id,
    );
  }

  listWorkflowAgents(runId: string): WorkflowAgent[] {
    return this.#all<WorkflowAgentRow>(
      `SELECT * FROM workflow_agents WHERE run_id = ? ORDER BY idx, rowid`,
      runId,
    ).map(toWorkflowAgent);
  }

  /** Journal lookup on rerun: the source run's row for a call key. First call wins. */
  findWorkflowAgent(runId: string, key: string): WorkflowAgent | undefined {
    const r = this.#get<WorkflowAgentRow>(
      `SELECT * FROM workflow_agents WHERE run_id = ? AND key = ? ORDER BY idx, rowid LIMIT 1`,
      runId,
      key,
    );
    return r && toWorkflowAgent(r);
  }

  // ---- keyword search -------------------------------------------------------

  /**
   * Index (or re-index) one message. Idempotent by delete-then-insert: a message is
   * re-indexed on every streaming update, and `messages_fts` is a standalone table
   * with no unique constraint to lean on, so the delete is what stops a supervisor
   * message from appearing once per round of its turn.
   *
   * A message with no prose contributes no row at all — an empty row would match
   * nothing but would still have to be walked.
   */
  indexMessage(m: Message): void {
    this.#run(`DELETE FROM messages_fts WHERE message_id = ?`, m.id);
    const text = indexableText(m.parts);
    if (!text) return;
    this.#run(
      `INSERT INTO messages_fts (text, message_id, session_id) VALUES (?, ?, ?)`,
      text,
      m.id,
      m.sessionId,
    );
  }

  /**
   * Keyword search over transcripts (spec §17 — FTS, no embeddings).
   *
   * Ordered by relevance and tie-broken by `(created_at DESC, message_id)` rather
   * than by anything FTS-internal, so a rebuilt index returns results in the same
   * order as an incrementally built one (plan T8.9). An FTS syntax error becomes a
   * 400 naming the query: the user typed it, and a bare "failed" would leave them
   * guessing which character SQLite objected to.
   */
  searchMessages(query: string, opts: { sessionId?: string; limit?: number } = {}): SearchHit[] {
    const limit = opts.limit ?? 20;
    const scoped = opts.sessionId !== undefined;
    const sql = `SELECT messages_fts.message_id AS message_id,
                        messages_fts.session_id AS session_id,
                        snippet(messages_fts, 0, '', '', '…', 24) AS snippet,
                        m.created_at AS created_at
                   FROM messages_fts
                   JOIN messages m ON m.id = messages_fts.message_id
                  WHERE messages_fts MATCH ?
                    ${scoped ? "AND messages_fts.session_id = ?" : ""}
                  ORDER BY rank, m.created_at DESC, messages_fts.message_id
                  LIMIT ?`;
    const params: (string | number)[] = scoped ? [query, opts.sessionId!, limit] : [query, limit];
    let rows: {
      message_id: string;
      session_id: string;
      snippet: string;
      created_at: number;
    }[];
    try {
      rows = this.#all(sql, ...params);
    } catch (e) {
      throw new BadRequestError(
        `search query ${JSON.stringify(query)} is not valid FTS5 syntax ` +
          `(${e instanceof Error ? e.message : String(e)}). ` +
          `Quote a phrase as "like this"; bare ", *, ^, : and NEAR are operators.`,
      );
    }
    return rows.map((r) => ({
      messageId: r.message_id,
      sessionId: r.session_id,
      snippet: r.snippet,
      createdAt: r.created_at,
    }));
  }

  /**
   * Rebuild the whole index from `messages`.
   *
   * Deliberately implemented as "clear, then run `indexMessage` over every row in
   * `(created_at, rowid)` order" rather than as a bulk INSERT..SELECT: sharing the
   * one projection function is what makes a rebuild produce results identical to
   * incremental indexing, which is T8.9's acceptance criterion. A second projection
   * here would be a second thing to keep in sync, and the drift would only ever show
   * up as search results that quietly differ after a rebuild.
   */
  rebuildSearchIndex(): void {
    this.#run(`DELETE FROM messages_fts`);
    const rows = this.#all<MessageRow>(`SELECT * FROM messages ORDER BY created_at, rowid`);
    for (const r of rows) this.indexMessage(toMessage(r));
  }

  // ---- command-history memory ----------------------------------------------

  /**
   * Append one finished command with its tag/dir junction rows and FTS row, in
   * one transaction — a half-recorded command (history row without its tags)
   * would silently skew every popularity query that joins them.
   */
  recordCommand(r: CommandRecord): void {
    const tx = this.#db.transaction(() => {
      const info = this.#db.prepare(
        `INSERT INTO command_history
           (session_id, ts, repo, cmd, tags, exit_code, duration_ms, output_head,
            spill_path, source, message_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      ).run(
        r.sessionId,
        r.ts,
        r.repo,
        r.cmd,
        r.tags,
        r.exitCode,
        r.durationMs,
        r.outputHead,
        r.spillPath,
        r.source,
        r.messageId ?? null,
      );
      const id = Number(info.lastInsertRowid);
      for (const tag of r.tagList) {
        this.#run(`INSERT INTO command_tags (command_id, tag) VALUES (?, ?)`, id, tag);
      }
      for (const dir of r.dirs) {
        this.#run(`INSERT INTO command_dirs (command_id, rel_dir) VALUES (?, ?)`, id, dir);
      }
      this.#run(
        `INSERT INTO command_history_fts (cmd, tags, output_head, command_id)
         VALUES (?, ?, ?, ?)`,
        r.cmd,
        r.tags,
        r.outputHead,
        id,
      );
    });
    tx();
  }

  commandTagRows(
    repo: string,
    opts: { dir?: string; sinceTs?: number } = {},
  ): CommandTagRow[] {
    const conds = [`h.repo = ?`];
    const params: (string | number)[] = [repo];
    if (opts.sinceTs !== undefined) {
      conds.push(`h.ts >= ?`);
      params.push(opts.sinceTs);
    }
    if (opts.dir !== undefined) {
      conds.push(
        `EXISTS (SELECT 1 FROM command_dirs d
                  WHERE d.command_id = h.id AND (d.rel_dir = ? OR d.rel_dir LIKE ? || '/%'))`,
      );
      params.push(opts.dir, opts.dir);
    }
    return this.#all<{ tag: string; ts: number; exit_code: number | null }>(
      `SELECT t.tag AS tag, h.ts AS ts, h.exit_code AS exit_code
         FROM command_history h JOIN command_tags t ON t.command_id = h.id
        WHERE ${conds.join(" AND ")}`,
      ...params,
    ).map((r) => ({ tag: r.tag, ts: r.ts, exitCode: r.exit_code }));
  }

  /**
   * How many distinct repos the memory holds, and how many of them use each tag.
   *
   * The contrast the priming note is ranked against: a tag every project uses names
   * a TOOL (`git`, `bun`, `rg`) and reusing it was never in question, while a tag
   * only this project uses names its subject and is the vocabulary worth sharing.
   * One pass, because the note is built per session and both halves come from the
   * same scan.
   */
  tagSpread(sinceTs?: number): { repos: number; byTag: Map<string, number> } {
    const where = sinceTs === undefined ? "" : ` WHERE h.ts >= ${Number(sinceTs)}`;
    const repos = this.#get<{ n: number }>(
      `SELECT COUNT(DISTINCT h.repo) AS n FROM command_history h${where}`,
    )?.n ?? 0;
    const byTag = new Map<string, number>();
    for (
      const r of this.#all<{ tag: string; repos: number }>(
        `SELECT t.tag AS tag, COUNT(DISTINCT h.repo) AS repos
           FROM command_history h JOIN command_tags t ON t.command_id = h.id${where}
          GROUP BY t.tag`,
      )
    ) byTag.set(r.tag, r.repos);
    return { repos, byTag };
  }

  /**
   * Tag diversity per day — what `bough tags stats` reports.
   *
   * The measurement the priming note and every prompt change need and did not have:
   * whether the model is naming MORE things or fewer. `distinctTags` against
   * `tagUses` is the vocabulary; `tagged` against `commands` is the coverage a bare
   * `sh` leg costs. Both are per day so a change on a date shows as a step.
   *
   * Grouped in SQLite's local time, because the question is "what did I do on
   * Tuesday" and a UTC day boundary answers a different one.
   */
  tagDiversityByDay(sinceTs: number, repo?: string): TagDiversityDay[] {
    const scope = repo === undefined ? "" : " AND h.repo = ?";
    const params: (string | number)[] = repo === undefined ? [sinceTs] : [sinceTs, repo];
    return this.#all<{
      day: string;
      sessions: number;
      commands: number;
      tagged: number;
      distinct_tags: number;
      distinct_refs: number;
      tag_uses: number;
    }>(
      `WITH d AS (
         SELECT h.id AS id, h.session_id AS session_id, h.tags AS tags,
                date(h.ts / 1000, 'unixepoch', 'localtime') AS day
           FROM command_history h WHERE h.ts >= ?${scope}
       )
       SELECT d.day AS day,
              COUNT(DISTINCT d.session_id) AS sessions,
              COUNT(DISTINCT d.id) AS commands,
              COUNT(DISTINCT CASE WHEN d.tags <> '' THEN d.id END) AS tagged,
              COUNT(DISTINCT CASE WHEN instr(t.tag, '.') = 0 THEN t.tag END) AS distinct_tags,
              COUNT(DISTINCT CASE WHEN instr(t.tag, '.') > 0 THEN t.tag END) AS distinct_refs,
              COUNT(t.tag) AS tag_uses
         FROM d LEFT JOIN command_tags t ON t.command_id = d.id
        GROUP BY d.day ORDER BY d.day DESC`,
      ...params,
    ).map((r) => ({
      day: r.day,
      sessions: r.sessions,
      commands: r.commands,
      tagged: r.tagged,
      distinctTags: r.distinct_tags,
      distinctRefs: r.distinct_refs,
      tagUses: r.tag_uses,
    }));
  }

  /**
   * The program a supervisor message ran, or null.
   *
   * The other half of `command_history.message_id`: a recalled command reaches the
   * ROUND that used it. Reads the first `run_steps` call in the message's parts —
   * one program per round is the whole design (spec §2), so "first" is "the one".
   * Null covers every ordinary absence: a row from before the column, a message a
   * compaction dropped, a turn that ran no program.
   */
  programForMessage(messageId: string): string | null {
    const row = this.#get<{ parts: string }>(
      `SELECT parts FROM messages WHERE id = ?`,
      messageId,
    );
    if (!row) return null;
    try {
      const parts = JSON.parse(row.parts) as { type: string; input?: { code?: unknown } }[];
      for (const p of parts) {
        if (p.type === "tool_call" && typeof p.input?.code === "string") return p.input.code;
      }
    } catch {
      // A part list that will not parse is a corrupt row, not a crash for a reader.
    }
    return null;
  }

  /** Commands recorded under one tag, newest first — `bough tags show`. */
  commandsForTag(tag: string, opts: { repo?: string; limit?: number } = {}): TaggedCommand[] {
    const scope = opts.repo === undefined ? "" : " AND h.repo = ?";
    const params: (string | number)[] = opts.repo === undefined ? [tag] : [tag, opts.repo];
    return this.#all<{
      ts: number;
      repo: string;
      cmd: string;
      tags: string;
      exit_code: number | null;
      duration_ms: number | null;
      session_id: string;
      message_id: string | null;
    }>(
      `SELECT h.ts AS ts, h.repo AS repo, h.cmd AS cmd, h.tags AS tags,
              h.exit_code AS exit_code, h.duration_ms AS duration_ms,
              h.session_id AS session_id, h.message_id AS message_id
         FROM command_history h JOIN command_tags t ON t.command_id = h.id
        WHERE t.tag = ?${scope}
        ORDER BY h.ts DESC LIMIT ${Math.max(1, Math.trunc(opts.limit ?? 20))}`,
      ...params,
    ).map((r) => ({
      ts: r.ts,
      repo: r.repo,
      cmd: r.cmd,
      tags: r.tags,
      exitCode: r.exit_code,
      durationMs: r.duration_ms,
      sessionId: r.session_id,
      messageId: r.message_id,
    }));
  }
}

/**
 * Open the database, creating its parent directory when it does not exist.
 *
 * The path resolves through `paths.dbPath()` — `BOUGH_DB`, else `<BOUGH_HOME>/bough.db`
 * — so a test sets one env var and gets a hermetic database, and the rewrite never
 * opens the live install's file (plan §2). `":memory:"` needs no directory.
 */
export function openDb(path: string = dbPath(), opts: DbOptions = {}): SqliteDb {
  if (path !== ":memory:" && !path.startsWith("file:")) {
    mkdirSync(dirname(path), { recursive: true });
  }
  return new SqliteDb(path, opts);
}
