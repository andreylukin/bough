/**
 * SQLite persistence for sessions, messages, and turns. Tree history lives here:
 * sessions form a parent_id forest and a session's "thread" is the concatenation of
 * messages along the root→self path (see threadFor).
 *
 * Driver decision: plain SQL over Deno's built-in `node:sqlite` (DatabaseSync),
 * wrapped in a thin typed layer below. We deliberately skip Drizzle — on Deno its
 * SQLite story means pulling a second native driver (better-sqlite3/libsql) when the
 * runtime already ships one, and our query surface is a handful of statements that read
 * clearer as SQL. The Zod schemas in schema/parts.ts already give us the type safety
 * Drizzle would.
 *
 * Storage: JSON columns for message `parts`; booleans as 0/1; timestamps as epoch ms.
 * DB file at ~/.bough/bough.db, overridable via BOUGH_DB (":memory:" for tests).
 */
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { boughHome } from "../paths.ts";
import { mkdirSync } from "node:fs";
import type { Message, Part, Role, Session, SessionKind } from "../schema/parts.ts";

const SCHEMA = `
CREATE TABLE IF NOT EXISTS sessions (
  id          TEXT PRIMARY KEY,
  parent_id   TEXT REFERENCES sessions(id),
  title       TEXT NOT NULL,
  kind        TEXT NOT NULL,
  created_at  INTEGER NOT NULL,
  workspace   TEXT,                   -- the session's workspace root; null = BOUGH_WORKSPACE/cwd.
                                      -- VM (guest-owned) mode: permanently the ORIGIN dir — the
                                      -- working copy is the guest clone, and legacy rows pointing
                                      -- at ~/.bough/workspaces worktrees are rewritten to origin_dir
                                      -- at startup (see #migrate). Host-worktree mode still repoints
                                      -- this at the session worktree on the first turn.
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
-- Recurring agent runs (see schedules.ts): each due fire creates a fresh root
-- session titled from the schedule and starts a turn with its prompt.
CREATE TABLE IF NOT EXISTS schedules (
  id          TEXT PRIMARY KEY,
  title       TEXT NOT NULL,
  prompt      TEXT NOT NULL,
  workspace   TEXT,                   -- null = chat-only sessions (no sandbox root)
  spec        TEXT NOT NULL,          -- "every:<N><m|h|d>" | "daily@HH:MM" (local)
  enabled     INTEGER NOT NULL,       -- 0/1
  created_at  INTEGER NOT NULL,
  last_run_at INTEGER,
  next_run_at INTEGER NOT NULL
);
-- Workflow runs (see workflow.ts): a script orchestrating many subagents. The
-- script text is persisted verbatim (also mirrored to ~/.bough/workflows/<id>.js
-- for out-of-band editing); each agent() call journals into workflow_agents so a
-- rerun can replay unchanged calls from cache instead of re-running them.
CREATE TABLE IF NOT EXISTS workflows (
  id           TEXT PRIMARY KEY,
  session_id   TEXT NOT NULL REFERENCES sessions(id),
  name         TEXT NOT NULL,
  description  TEXT NOT NULL,
  script       TEXT NOT NULL,
  phases       TEXT NOT NULL,         -- JSON [{title, detail?}] from the script's meta
  status       TEXT NOT NULL,         -- running | paused | done | error | stopped | orphaned
  current_phase TEXT,
  result       TEXT,                  -- JSON: the script's return value (status done)
  error        TEXT,                  -- the failure message (status error)
  args         TEXT,                  -- JSON: the run's args input
  resume_of    TEXT,                  -- run id this rerun replays its journal from
  created_at   INTEGER NOT NULL,
  finished_at  INTEGER
);
CREATE TABLE IF NOT EXISTS workflow_agents (
  id          TEXT PRIMARY KEY,
  run_id      TEXT NOT NULL REFERENCES workflows(id),
  idx         INTEGER NOT NULL,       -- call order within the run
  key         TEXT NOT NULL,          -- hash(prompt|opts) — the journal replay key
  label       TEXT NOT NULL,
  phase       TEXT,
  prompt      TEXT NOT NULL,
  model       TEXT,
  status      TEXT NOT NULL,          -- queued | running | done | error | stopped | cached
  result      TEXT,                   -- the agent's report text (done/cached)
  session_id  TEXT,                   -- the subagent session (TUI drill-in)
  started_at  INTEGER NOT NULL,
  finished_at INTEGER
);
CREATE INDEX IF NOT EXISTS workflow_agents_run ON workflow_agents(run_id, idx);
-- Durable key/value notes the program writes for ITSELF (see state.ts). Scoped to
-- the ROOT session of a lineage so forks, compactions and subagents of the same
-- conversation read the same store — the point is surviving a context that the
-- turn loop will eventually truncate or compact away.
CREATE TABLE IF NOT EXISTS session_state (
  root_id     TEXT NOT NULL,
  key         TEXT NOT NULL,
  value       TEXT NOT NULL,         -- JSON: whatever the program stored
  updated_at  INTEGER NOT NULL,
  PRIMARY KEY (root_id, key)
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
  origin_dir: string | null;
  base: string | null;
  origin_id: string | null;
  origin_message_id: string | null;
  deprecated_at: number | null;
  archived_at: number | null;
  context_tokens: number | null;
  cached_tokens: number | null;
  last_llm_at: number | null;
  model: string | null;
  effort: string | null;
  prompt_dir: string | null;
  draft: string | null;
  outcome_ok: number | null;
  outcome_check_passed: number | null;
};

/**
 * The runtime (non-wire) facts a session carries for the turn runner: its explicit
 * workspace (null = fall back to BOUGH_WORKSPACE/cwd) and its persisted snapshot base.
 * Kept off the wire `Session` type so the UI mirror in schema/parts.ts is untouched.
 */
interface SessionRuntime {
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

/** A recurring agent run: title/prompt/workspace for the session each fire creates. */
export interface Schedule {
  id: string;
  title: string;
  prompt: string;
  workspace: string | null;
  /** "every:<N><m|h|d>" or "daily@HH:MM" — parsed by schedules.ts, stored verbatim. */
  spec: string;
  enabled: boolean;
  createdAt: number;
  lastRunAt: number | null;
  nextRunAt: number;
}
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
};

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
  };
}

export type WorkflowStatus = "running" | "paused" | "done" | "error" | "stopped" | "orphaned";
/** `queued` = journaled but parked on the run's concurrency semaphore: no
 * subagent session yet, and its clock has not started. It used to report
 * `running` with a ticking elapsed, so a saturated run looked like N agents
 * working when only `concurrency()` of them were. */
export type WorkflowAgentStatus =
  | "queued"
  | "running"
  | "done"
  | "error"
  | "stopped"
  | "cached";

/** One workflow run: the script, its meta (name/description/phases), and outcome. */
export interface WorkflowRun {
  id: string;
  sessionId: string;
  name: string;
  description: string;
  script: string;
  /** From the script's meta: [{title, detail?}]. */
  phases: { title: string; detail?: string }[];
  status: WorkflowStatus;
  currentPhase: string | null;
  /** The script's return value (status "done"). */
  result: unknown;
  error: string | null;
  args: unknown;
  /** Run id this rerun replays its journal from. */
  resumeOf: string | null;
  createdAt: number;
  finishedAt: number | null;
}

/** One agent() call's journal row — the unit the TUI drills into and reruns replay. */
export interface WorkflowAgent {
  id: string;
  runId: string;
  idx: number;
  key: string;
  label: string;
  phase: string | null;
  prompt: string;
  model: string | null;
  status: WorkflowAgentStatus;
  /** The agent's report text (done), or the cached copy of it (cached). */
  result: string | null;
  /** The subagent session backing this call (absent for cached replays). */
  sessionId: string | null;
  startedAt: number;
  finishedAt: number | null;
}

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
  status: string;
  result: string | null;
  session_id: string | null;
  started_at: number;
  finished_at: number | null;
};

function toWorkflow(r: WorkflowRow): WorkflowRun {
  return {
    id: r.id,
    sessionId: r.session_id,
    name: r.name,
    description: r.description,
    script: r.script,
    phases: JSON.parse(r.phases),
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
    sessionId: r.session_id,
    startedAt: r.started_at,
    finishedAt: r.finished_at,
  };
}

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
    ...(r.origin_dir ? { originDir: r.origin_dir } : {}),
    ...(r.origin_id ? { originId: r.origin_id } : {}),
    ...(r.origin_message_id ? { originMessageId: r.origin_message_id } : {}),
    ...(r.deprecated_at != null ? { deprecatedAt: r.deprecated_at } : {}),
    ...(r.archived_at != null ? { archivedAt: r.archived_at } : {}),
    ...(r.model ? { model: r.model } : {}),
    ...(r.effort ? { effort: r.effort } : {}),
    ...(r.prompt_dir ? { promptDir: r.prompt_dir } : {}),
    // Prompt-cache visibility: last prompt size, its cached share, and when the
    // last LLM round finished (the client derives warm/cold from this + the TTL).
    ...(r.context_tokens != null ? { contextTokens: r.context_tokens } : {}),
    ...(r.cached_tokens != null ? { cachedTokens: r.cached_tokens } : {}),
    ...(r.last_llm_at != null ? { lastLlmAt: r.last_llm_at } : {}),
    ...(r.draft != null ? { draft: r.draft } : {}),
    // Delegation outcome (subagents only; see setSessionOutcome).
    ...(r.outcome_ok != null ? { outcomeOk: r.outcome_ok === 1 } : {}),
    ...(r.outcome_check_passed != null ? { outcomeCheckPassed: r.outcome_check_passed === 1 } : {}),
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

  /** Session ids whose workspace column was rewritten to the origin by THIS
   *  open's legacy migration. Nothing consumes it since the worktree store went
   *  away (there is no leftover worktree to retire — scripts/bough deletes the
   *  whole store); the migration also logs, so this is only for callers that
   *  want the list rather than the line. */
  readonly migratedLegacyWorkspaces: string[] = [];

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
        "origin_dir TEXT",
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
        "prompt_dir TEXT",
        "draft TEXT",
        "outcome_ok INTEGER",
        "outcome_check_passed INTEGER",
      ]
    ) {
      try {
        this.#db.exec(`ALTER TABLE sessions ADD COLUMN ${col}`);
      } catch {
        // column already exists
      }
    }
    try {
      this.#db.exec(`ALTER TABLE turns ADD COLUMN first_output_at INTEGER`);
    } catch {
      // column already exists
    }
    this.#migrateLegacyWorkspaces();
  }

  /**
   * The workspace column now always holds the user's real checkout — sessions run
   * in place, and the per-session worktree store under ~/.bough/workspaces is gone
   * (scripts/bough reaps it on update). Rows still pointing into that store would
   * resume a session into a directory that no longer exists, so rewrite them to the
   * session's origin_dir. Unconditional: there is no mode left in which a row under
   * the workspaces root is correct. One-shot in practice — after the rewrite the
   * rows no longer match, so the next open finds nothing.
   */
  #migrateLegacyWorkspaces(): void {
    const root = (Deno.env.get("BOUGH_SUBAGENT_BASE") ??
      `${Deno.env.get("HOME") ?? ""}/.bough/workspaces`).replace(/\/+$/, "");
    if (root === "/.bough/workspaces") return; // no HOME — nothing sane to match
    const rows = this.#db
      .prepare(
        `SELECT id, workspace, origin_dir FROM sessions
         WHERE workspace IS NOT NULL AND origin_dir IS NOT NULL`,
      )
      .all() as Array<{ id: string; workspace: string; origin_dir: string }>;
    const legacy = rows.filter((r) => r.workspace.startsWith(root + "/"));
    if (legacy.length === 0) return;
    const set = this.#db.prepare(`UPDATE sessions SET workspace = ? WHERE id = ?`);
    for (const r of legacy) set.run(r.origin_dir, r.id);
    this.migratedLegacyWorkspaces.push(...legacy.map((r) => r.id));
    console.log(
      `migrated ${legacy.length} legacy workspace row(s) to their origin dir: ` +
        legacy.map((r) => `${r.id} → ${r.origin_dir}`).join(", "),
    );
  }

  close(): void {
    this.#db.close();
  }

  // sessions ----------------------------------------------------------------

  createSession(s: Session): Session {
    this.#db
      .prepare(
        `INSERT INTO sessions (id, parent_id, title, kind, created_at, workspace, origin_dir, origin_id, origin_message_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        s.id,
        s.parentId,
        s.title,
        s.kind,
        s.createdAt,
        s.workspace ?? null,
        s.originDir ?? null,
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

  /** Per-session system-prompt override dir; null clears back to the process default.
   * Lets a prompt variant be pinned per session (bough exec --prompt-dir) with no
   * server restart — the turn runner reads it via getSession each turn. */
  setSessionPromptDir(id: string, promptDir: string | null): void {
    this.#db.prepare(`UPDATE sessions SET prompt_dir = ? WHERE id = ?`).run(promptDir, id);
  }

  /** Persist a finished subagent's delegation outcome (see subagent.ts buildResult):
   * the in-band agent() result only reaches the parent program, so the branch row
   * carries {ok, checkPassed} for the UI to render failed/check-failed states. */
  setSessionOutcome(id: string, ok: boolean, checkPassed: boolean): void {
    this.#db
      .prepare(`UPDATE sessions SET outcome_ok = ?, outcome_check_passed = ? WHERE id = ?`)
      .run(ok ? 1 : 0, checkPassed ? 1 : 0, id);
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

  /** Archived (soft-deleted) sessions, newest first — the reveal/restore drawer. */
  listArchivedSessions(): Session[] {
    const rows = this.#db
      .prepare(`SELECT * FROM sessions WHERE archived_at IS NOT NULL ORDER BY created_at DESC`)
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

  /** Undo archiveSession: the row returns to the sidebar list. */
  unarchiveSession(id: string): void {
    this.#db.prepare(`UPDATE sessions SET archived_at = NULL WHERE id = ?`).run(id);
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
      for (const t of ["message_embeddings", "turns"]) {
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

  // schedules ---------------------------------------------------------------

  createSchedule(s: Schedule): Schedule {
    this.#db
      .prepare(
        `INSERT INTO schedules (id, title, prompt, workspace, spec, enabled, created_at, last_run_at, next_run_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        s.id,
        s.title,
        s.prompt,
        s.workspace,
        s.spec,
        s.enabled ? 1 : 0,
        s.createdAt,
        s.lastRunAt,
        s.nextRunAt,
      );
    return s;
  }

  getSchedule(id: string): Schedule | undefined {
    const r = this.#db.prepare(`SELECT * FROM schedules WHERE id = ?`).get(id) as
      | ScheduleRow
      | undefined;
    return r && toSchedule(r);
  }

  listSchedules(): Schedule[] {
    const rows = this.#db
      .prepare(`SELECT * FROM schedules ORDER BY created_at, rowid`)
      .all() as ScheduleRow[];
    return rows.map(toSchedule);
  }

  /** Enabled schedules whose next_run_at has passed — the ticker's due set. */
  dueSchedules(now: number): Schedule[] {
    const rows = this.#db
      .prepare(
        `SELECT * FROM schedules WHERE enabled = 1 AND next_run_at <= ? ORDER BY next_run_at`,
      )
      .all(now) as ScheduleRow[];
    return rows.map(toSchedule);
  }

  /** Overwrite the caller-recomputed fields (PATCH merges into the full row first). */
  updateSchedule(s: Schedule): void {
    this.#db
      .prepare(
        `UPDATE schedules SET title = ?, prompt = ?, workspace = ?, spec = ?, enabled = ?, next_run_at = ? WHERE id = ?`,
      )
      .run(s.title, s.prompt, s.workspace, s.spec, s.enabled ? 1 : 0, s.nextRunAt, s.id);
  }

  /** Stamp a fire: when it ran and when it runs next. */
  markScheduleRun(id: string, lastRunAt: number, nextRunAt: number): void {
    this.#db
      .prepare(`UPDATE schedules SET last_run_at = ?, next_run_at = ? WHERE id = ?`)
      .run(lastRunAt, nextRunAt, id);
  }

  deleteSchedule(id: string): void {
    this.#db.prepare(`DELETE FROM schedules WHERE id = ?`).run(id);
  }

  // session state (durable key/value notes — see state.ts) --------------------

  /** The stored JSON text for a key, or undefined when unset. */
  getState(rootId: string, key: string): string | undefined {
    const r = this.#db
      .prepare(`SELECT value FROM session_state WHERE root_id = ? AND key = ?`)
      .get(rootId, key) as { value: string } | undefined;
    return r?.value;
  }

  /** Upsert: a re-set overwrites in place and re-stamps updated_at. */
  setState(rootId: string, key: string, value: string, now: number): void {
    this.#db
      .prepare(
        `INSERT INTO session_state (root_id, key, value, updated_at) VALUES (?, ?, ?, ?)
         ON CONFLICT(root_id, key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at`,
      )
      .run(rootId, key, value, now);
  }

  /** Keys only (with sizes) — listing never drags whole values into context. */
  listState(rootId: string): { key: string; bytes: number; updatedAt: number }[] {
    const rows = this.#db
      .prepare(
        `SELECT key, length(value) AS bytes, updated_at FROM session_state
         WHERE root_id = ? ORDER BY key`,
      )
      .all(rootId) as { key: string; bytes: number; updated_at: number }[];
    return rows.map((r) => ({ key: r.key, bytes: r.bytes, updatedAt: r.updated_at }));
  }

  /** True when a row was actually removed (the program learns "there was nothing"). */
  deleteState(rootId: string, key: string): boolean {
    const before = this.getState(rootId, key) !== undefined;
    this.#db.prepare(`DELETE FROM session_state WHERE root_id = ? AND key = ?`).run(rootId, key);
    return before;
  }

  // workflows ---------------------------------------------------------------

  createWorkflow(w: WorkflowRun): WorkflowRun {
    this.#db
      .prepare(
        `INSERT INTO workflows (id, session_id, name, description, script, phases, status, current_phase, result, error, args, resume_of, created_at, finished_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        w.id,
        w.sessionId,
        w.name,
        w.description,
        w.script,
        JSON.stringify(w.phases),
        w.status,
        w.currentPhase,
        w.result === null ? null : JSON.stringify(w.result),
        w.error,
        w.args === null || w.args === undefined ? null : JSON.stringify(w.args),
        w.resumeOf,
        w.createdAt,
        w.finishedAt,
      );
    return w;
  }

  getWorkflow(id: string): WorkflowRun | undefined {
    const r = this.#db.prepare(`SELECT * FROM workflows WHERE id = ?`).get(id) as
      | WorkflowRow
      | undefined;
    return r && toWorkflow(r);
  }

  listWorkflows(sessionId?: string): WorkflowRun[] {
    const rows =
      (sessionId
        ? this.#db.prepare(`SELECT * FROM workflows WHERE session_id = ? ORDER BY created_at DESC`)
          .all(sessionId)
        : this.#db.prepare(`SELECT * FROM workflows ORDER BY created_at DESC`)
          .all()) as WorkflowRow[];
    return rows.map(toWorkflow);
  }

  /** Overwrite the run's mutable outcome fields (status/phase/result/error/finished). */
  updateWorkflow(
    id: string,
    patch: {
      status?: WorkflowStatus;
      currentPhase?: string | null;
      result?: unknown;
      error?: string | null;
      finishedAt?: number | null;
    },
  ): void {
    const cur = this.getWorkflow(id);
    if (!cur) return;
    this.#db
      .prepare(
        `UPDATE workflows SET status = ?, current_phase = ?, result = ?, error = ?, finished_at = ? WHERE id = ?`,
      )
      .run(
        patch.status ?? cur.status,
        patch.currentPhase !== undefined ? patch.currentPhase : cur.currentPhase,
        patch.result !== undefined
          ? (patch.result === null ? null : JSON.stringify(patch.result))
          : (cur.result === null ? null : JSON.stringify(cur.result)),
        patch.error !== undefined ? patch.error : cur.error,
        patch.finishedAt !== undefined ? patch.finishedAt : cur.finishedAt,
        id,
      );
  }

  /** Running/paused runs — the orphan-recovery set after a server restart. */
  unfinishedWorkflows(): WorkflowRun[] {
    const rows = this.#db
      .prepare(`SELECT * FROM workflows WHERE status IN ('running', 'paused')`)
      .all() as WorkflowRow[];
    return rows.map(toWorkflow);
  }

  createWorkflowAgent(a: WorkflowAgent): WorkflowAgent {
    this.#db
      .prepare(
        `INSERT INTO workflow_agents (id, run_id, idx, key, label, phase, prompt, model, status, result, session_id, started_at, finished_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        a.id,
        a.runId,
        a.idx,
        a.key,
        a.label,
        a.phase,
        a.prompt,
        a.model,
        a.status,
        a.result,
        a.sessionId,
        a.startedAt,
        a.finishedAt,
      );
    return a;
  }

  updateWorkflowAgent(
    id: string,
    patch: {
      status?: WorkflowAgentStatus;
      result?: string | null;
      sessionId?: string | null;
      /** Reset when a queued agent actually starts, so elapsed excludes queue time. */
      startedAt?: number;
      finishedAt?: number | null;
    },
  ): void {
    const r = this.#db.prepare(`SELECT * FROM workflow_agents WHERE id = ?`).get(id) as
      | WorkflowAgentRow
      | undefined;
    if (!r) return;
    this.#db
      .prepare(
        `UPDATE workflow_agents SET status = ?, result = ?, session_id = ?, started_at = ?, finished_at = ? WHERE id = ?`,
      )
      .run(
        patch.status ?? r.status,
        patch.result !== undefined ? patch.result : r.result,
        patch.sessionId !== undefined ? patch.sessionId : r.session_id,
        patch.startedAt !== undefined ? patch.startedAt : r.started_at,
        patch.finishedAt !== undefined ? patch.finishedAt : r.finished_at,
        id,
      );
  }

  listWorkflowAgents(runId: string): WorkflowAgent[] {
    const rows = this.#db
      .prepare(`SELECT * FROM workflow_agents WHERE run_id = ? ORDER BY idx`)
      .all(runId) as WorkflowAgentRow[];
    return rows.map(toWorkflowAgent);
  }
}

/** Resolve the DB path: BOUGH_DB override, else ~/.bough/bough.db (dir created). */
function defaultDbPath(): string {
  const override = Deno.env.get("BOUGH_DB");
  if (override) return override;
  const dir = boughHome();
  mkdirSync(dir, { recursive: true });
  return join(dir, "bough.db");
}

export function openDb(path: string = defaultDbPath()): Db {
  return new Db(path);
}
