/**
 * The subagent launch path — the one place a delegated session comes into being.
 *
 * THE INVARIANT THIS HOLDS: **a subagent starts from nothing but its task.** It is
 * a real session (`kind: "subagent"`) with `parentId: null`, and that null is the
 * whole feature. `db.threadFor` is "every ancestor's messages, then my own", so a
 * parent pointer would silently hand the child the spawner's entire conversation —
 * every earlier turn, every tool dump, every abandoned plan. With `parentId: null`
 * the child's thread is exactly the one message this module seeds, which is why the
 * task string has to carry every path, constraint and acceptance criterion: there is
 * no earlier conversation to consult and nobody to ask (spec §7).
 *
 * Three things DO cross the boundary, and each is deliberate:
 *
 *   1. **The lineage edge.** `originId` / `originMessageId` point at the spawning
 *      session and the supervisor message that was in flight when the program called
 *      `agent()`. Nothing else records that a branch exists — visibility is derived,
 *      not stored (spec §4), so a subagent with no origin would be invisible in every
 *      listing at once. This is also the edge the tree view draws.
 *   2. **The checkout.** The child works in the SAME workspace as its spawner. There
 *      is no per-agent worktree and nothing to merge afterwards (spec §17) — a
 *      subagent's writes are already present when it reports. The corollary the
 *      spawner owns: concurrent children must be given disjoint files, and `patch`'s
 *      hash anchoring is what turns a violation into a reported conflict rather than
 *      a silent overwrite.
 *   3. **The MCP grant.** The human's grant to a spawner extends to the subagents
 *      doing parts of that same granted work. It is captured HERE, at spawn time,
 *      by handing the child's turn a ctx carrying `mcpGrant` — so a later manual
 *      continuation of that branch, which starts from the server's own `AppCtx`,
 *      does not inherit it (spec §7, `types.ts`).
 *
 * WHAT IS NOT HERE. This module launches; it does not decide how the launch is
 * awaited. `launchSubagent` returns the handle immediately *and* a promise for the
 * assembled result, because that is exactly the pair both delegation modes need: the
 * blocking `agent()` awaits the promise, the detached `spawn()` returns the handle
 * and lets the promise deliver a system note. Modes (T4.2), the width caps (T4.3)
 * and note delivery (T4.4) build on this and are not in this file yet. The depth cap
 * below IS here, because it is derived from the lineage this module writes and
 * because without it a self-delegating model recurses without bound.
 *
 * Ported from `src/subagent.ts`, which encodes the lineage rules. Deltas from that
 * port are marked `NOTE:`.
 */
import { AgentError } from "../errors.ts";
import type { Message, Session, Turn } from "../schema/parts.ts";
import type { AppCtx, Db, Effort, TurnCtx } from "../types.ts";
import { beginTurn, interruptTurn, type TurnDeps, type TurnOutcome } from "../turn/runner.ts";

// ---------------------------------------------------------------------------
// Caps that belong to lineage
// ---------------------------------------------------------------------------

/**
 * Nesting cap. A root (lineage depth 0) may spawn subagents (1), which may delegate
 * one level further (2); depth 2 is terminal (spec §7).
 *
 * NOTE: this is checked against the LINEAGE, not against `TurnCtx.depth`. The runner
 * sets `depth: 1` for any session of kind `subagent`, however deeply nested — it is a
 * tier flag that answers "may this turn spawn detached work?", not a hop count. Only
 * the `originId` chain knows how far down we actually are, and without walking it a
 * model that delegates to itself recurses until the machine gives out.
 */
export const MAX_SUBAGENT_DEPTH = 2;

/**
 * How many `subagent` hops separate this session from the top of its tree. 0 for a
 * root, fork or compaction. Pure over the database, so a caller can ask before it
 * commits to a launch.
 *
 * The hop cap is not paranoia about a well-formed tree — lineage is written once, at
 * spawn, so a cycle can only come from a bad write. It is what stops such a write
 * from hanging every later launch on an infinite walk.
 */
export function subagentDepth(db: Db, sessionId: string): number {
  let depth = 0;
  let cur = db.getSession(sessionId);
  while (cur?.kind === "subagent" && depth < 16) {
    depth++;
    cur = cur.originId ? db.getSession(cur.originId) : undefined;
  }
  return depth;
}

// ---------------------------------------------------------------------------
// Naming
// ---------------------------------------------------------------------------

/** What a session with neither a given name nor a usable task line is called. */
export const UNTITLED = "untitled";

/** The task-derived title's budget, in characters. */
export const TASK_STUB_CHARS = 40;

/**
 * A spawner-supplied name, cleaned for use as a branch title.
 *
 * Naming is required of the spawner for a reason (spec §7): during a fan-out "audit
 * the seatbelt profile" beats the first 40 characters of a 2KB briefing, and it is
 * stable from the instant the branch appears. Control characters and newlines are
 * stripped because this string is rendered straight into the rail, the finished card
 * and the session picker — a name with a `\n` in it would break every one of them.
 *
 * Returns `undefined` for a name that is absent or empty once cleaned, so the caller
 * falls back to the task stub rather than showing a blank branch.
 */
export function cleanSubagentName(name: unknown): string | undefined {
  if (name === undefined || name === null) return undefined;
  if (typeof name !== "string") {
    throw new AgentError(400, "agent/spawn(task, {name}): name must be a string");
  }
  // deno-lint-ignore no-control-regex
  const flat = name.replace(/[\x00-\x1f\x7f]/g, " ").replace(/\s+/g, " ").trim();
  if (!flat) return undefined;
  return flat.length <= 48 ? flat : `${flat.slice(0, 47).trimEnd()}…`;
}

/**
 * The default name: the task's first line, word-truncated to ~40 characters.
 *
 * Without it every branch of a parallel fan-out reads "untitled" until something
 * backfills a real name, and the user cannot tell four running siblings apart at the
 * moment they most need to. The cut lands on a word boundary unless that would throw
 * away most of the budget, in which case a hard cut reads better than two words.
 */
export function taskStubTitle(task: string): string {
  const line = task.trim().split("\n")[0]?.replace(/\s+/g, " ").trim() ?? "";
  if (!line) return UNTITLED;
  if (line.length <= TASK_STUB_CHARS) return line;
  const cut = line.slice(0, TASK_STUB_CHARS);
  const at = cut.lastIndexOf(" ");
  return `${(at > TASK_STUB_CHARS / 2 ? cut.slice(0, at) : cut).trimEnd()}…`;
}

// ---------------------------------------------------------------------------
// Shapes
// ---------------------------------------------------------------------------

/** What the spawner asked for — the `{name}` bag of `agent(task, {name})`. */
export interface SubagentOptions {
  /** The branch label. Absent = `taskStubTitle(task)`. */
  name?: string;
  /** Pin the child to a different model. Absent = the spawning turn's own. */
  model?: string;
  /** Thinking depth for the child. Absent = the spawning turn's own. */
  effort?: Effort;
}

/**
 * How a subagent's turn ended, as the spawner learns it.
 *
 * `status` is carried alongside `ok` because "failed" is not one fact: a child that
 * errored, one the user stopped, and one the server restarted under call for
 * different responses from the parent, and a bare boolean makes all three look the
 * same (plan T4.4's failure matrix).
 */
export interface SubagentResult {
  sessionId: string;
  title: string;
  /** The turn ran to completion: no error, no interrupt, no orphaning. */
  ok: boolean;
  status: "done" | "error" | "interrupted" | "orphaned";
  /** The child's final text — its whole report. Never empty (see `reportOf`). */
  report: string;
  /** Paths the child changed. Empty until the changes module is wired in (below). */
  changedFiles: string[];
}

/** The immediate handle, available before the child's turn has done anything. */
export interface SubagentHandle {
  sessionId: string;
  title: string;
}

/**
 * What a launch returns: the handle *now*, and the result *later*.
 *
 * Both parts are the point. A detached `spawn()` answers the program with the handle
 * and never awaits the promise; a blocking `agent()` awaits it; the workflow engine
 * needs the session id before completion so its journal row and the rail can point at
 * a branch while it is still running. One launch path, three consumers.
 */
export interface SubagentLaunch extends SubagentHandle {
  session: Session;
  /** The seeded task message — the child's ENTIRE thread at this instant. */
  taskMessage: Message;
  /** The child's pending supervisor message; its text becomes the report. */
  messageId: string;
  result: Promise<SubagentResult>;
}

/** How the child's turn is started. `beginTurn` satisfies this. */
export type BeginTurn = (
  ctx: AppCtx,
  sessionId: string,
  deps?: TurnDeps,
) => { message: Message; done: Promise<TurnOutcome> };

/** The injection seams, so a launch is drivable offline with no worker and no key. */
export interface LaunchDeps {
  /** Injected clock. Absent = `ctx.now`, then `Date.now`. */
  now?: () => number;
  /** The CHILD turn's deps: its program runner, granted host functions, registry. */
  turn?: TurnDeps;
  /** Defaults to `beginTurn`. */
  begin?: BeginTurn;
  /**
   * Wall-clock cap on the child's turn; an overrun is interrupted and reports
   * `status: "interrupted"` rather than hanging the spawner forever. Absent = the
   * `BOUGH_SUBAGENT_TIMEOUT_MS` env override, then 15 minutes.
   */
  timeoutMs?: number;
  /**
   * The paths the child changed, for its report. Absent = `[]`.
   *
   * A seam rather than a call into git, for two reasons: `server/changes.ts` (T8.8)
   * is the one module that owns "what changed since `sessions.base`", and duplicating
   * its diff here would give delegation a second answer to the same question. Until
   * it is wired, an empty list is the honest answer — the child's writes are in the
   * shared checkout either way, which is what the report says.
   */
  changedFiles?: (session: Session) => Promise<string[]> | string[];
}

// ---------------------------------------------------------------------------
// The launch
// ---------------------------------------------------------------------------

/** 15 minutes. Env-overridable so the timeout path is testable without waiting. */
function defaultTimeoutMs(): number {
  const n = Number(process.env["BOUGH_SUBAGENT_TIMEOUT_MS"]);
  return Number.isFinite(n) && n > 0 ? n : 15 * 60_000;
}

/**
 * Create the subagent session, seed its task, and start its turn.
 *
 * Ordering here is load-bearing. The session row lands and is announced before the
 * task message, so a client that reconciles by id never sees a message for a session
 * it has not heard of. The task message lands before the turn begins, so the child's
 * first round already has its briefing — `beginTurn` reads the thread synchronously.
 * And the handle is returned before the turn finishes, which is the difference
 * between a detached spawn and a blocking one.
 */
export function launchSubagent(
  ctx: TurnCtx,
  task: string,
  opts: SubagentOptions = {},
  deps: LaunchDeps = {},
): SubagentLaunch {
  const { db, bus } = ctx;
  const now = deps.now ?? ctx.now ?? Date.now;

  if (typeof task !== "string" || !task.trim()) {
    throw new AgentError(
      400,
      "agent/spawn(task): task must be a non-empty string — it is the subagent's " +
        "entire briefing, so it has to name the paths, the constraints and what done means",
    );
  }

  const spawner = db.getSession(ctx.sessionId);
  if (!spawner) throw new AgentError(404, `spawning session ${ctx.sessionId} not found`);

  const depth = subagentDepth(db, ctx.sessionId);
  if (depth >= MAX_SUBAGENT_DEPTH) {
    throw new AgentError(
      400,
      `delegation depth limit (${MAX_SUBAGENT_DEPTH}) reached: this session is already ` +
        `${depth} level(s) of subagent deep — do the remaining work here rather than ` +
        `delegating further`,
    );
  }

  // The spawner's own checkout, verbatim. `TurnCtx.workspace` is the path the
  // spawning turn actually resolved and is operating in, so recording it here makes
  // "the same checkout" a stored fact rather than a coincidence of two lookups.
  const runtime = db.getSessionRuntime(ctx.sessionId);
  const workspace = runtime.workspace ?? ctx.workspace;

  const title = cleanSubagentName(opts.name) ?? taskStubTitle(task);

  const session = db.createSession({
    id: crypto.randomUUID(),
    title,
    kind: "subagent",
    createdAt: now(),
    // The invariant, in one field: no inherited thread. See the module header.
    parentId: null,
    // The lineage edge — the only record that this branch exists (spec §4).
    originId: ctx.sessionId,
    originMessageId: ctx.messageId,
    workspace,
    // Which PROJECT this is for. Inherited rather than re-derived so a subagent of a
    // session whose workspace has moved still files under the original project.
    originDir: spawner.originDir ?? workspace,
    // `base` is deliberately unset. It is the sha a session's change set is measured
    // from, and the module that knows how to read one is T8.8 — inheriting the
    // spawner's would report the spawner's own work as the child's.
    base: null,
    // Not pinned: an inherited model is the spawning turn's default, not a decision
    // the user made about this branch (spec §4, §12). It reaches the child's turn
    // through `childCtx` below, so a later manual continuation follows the global
    // default like any other session.
    model: null,
    effort: null,
    draft: null,
  });
  bus.publish({ type: "session.created", sessionId: session.id, data: session });

  const taskMessage = db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "user",
    parts: [{ type: "text", text: task }],
    // Complete the moment it lands; `pending` is the supervisor's streaming flag.
    pending: false,
    createdAt: now(),
  });
  indexQuietly(db, taskMessage);
  bus.publish({ type: "message.started", sessionId: session.id, data: taskMessage });

  /**
   * The child's application context.
   *
   * Narrow on purpose: it carries the injected database, bus, clock and provider
   * client, the spawning turn's resolved model/effort as defaults, and the MCP grant
   * — and nothing that would tie the child to the spawner's turn. `runner.drive`
   * overwrites `sessionId`, `messageId`, `workspace`, `signal` and `depth` when it
   * builds the child's `TurnCtx`, so anything else here would be dead weight at best
   * and a stale abort signal at worst.
   */
  const childCtx: AppCtx & { mcpGrant?: string[] } = {
    db,
    bus,
    ...(ctx.llm ? { llm: ctx.llm } : {}),
    model: opts.model ?? ctx.model,
    ...((opts.effort ?? ctx.effort) ? { effort: opts.effort ?? ctx.effort } : {}),
    ...(ctx.now ? { now: ctx.now } : {}),
    ...(ctx.cheap ? { cheap: ctx.cheap } : {}),
    // Captured at spawn time — this is what makes the grant NOT follow a later
    // manual continuation of the branch (spec §7).
    ...(ctx.mcpGrant ? { mcpGrant: ctx.mcpGrant } : {}),
  };

  const begin = deps.begin ?? beginTurn;
  const { message, done } = begin(childCtx, session.id, deps.turn);

  // An overrun is interrupted rather than left to run: the spawner is holding a
  // promise, and a child that never ends is a turn that never ends above it too.
  const timer = setTimeout(
    () => interruptTurn(session.id, deps.turn?.registry),
    deps.timeoutMs ?? defaultTimeoutMs(),
  );

  const result = done
    .finally(() => clearTimeout(timer))
    .then(() => buildResult(ctx, session.id, message.id, deps))
    .then((r) => {
      // The turn runner already stamped `outcome_ok` on the row. Announcing it is
      // this module's job: without a `session.updated` the rail keeps rendering a
      // finished branch as live, and the tree never learns the branch failed.
      const updated = db.getSession(session.id);
      if (updated) bus.publish({ type: "session.updated", sessionId: session.id, data: updated });
      return r;
    });

  return {
    sessionId: session.id,
    title: session.title,
    session,
    taskMessage,
    messageId: message.id,
    result,
  };
}

// ---------------------------------------------------------------------------
// The result
// ---------------------------------------------------------------------------

/**
 * Assemble what the spawner is told, from what the child actually persisted.
 *
 * Read out of the database rather than taken from the in-memory `TurnOutcome`: the
 * report is the child's final text, and the turn row is what a restart would have
 * left behind. A child whose server died mid-turn has no outcome object at all, and
 * this path still produces a truthful `orphaned` result for it.
 */
export async function buildResult(
  ctx: Pick<TurnCtx, "db">,
  sessionId: string,
  messageId: string,
  deps: Pick<LaunchDeps, "changedFiles"> = {},
): Promise<SubagentResult> {
  const { db } = ctx;
  const session = db.getSession(sessionId);
  const status = finalStatus(db.turnForMessage(messageId));

  let changedFiles: string[] = [];
  if (deps.changedFiles && session) {
    try {
      changedFiles = [...(await deps.changedFiles(session))];
    } catch {
      // Reporting the diff is best-effort; the branch and its writes are intact
      // either way, and failing the whole delegation over a `git` hiccup would
      // discard a report the spawner needs.
    }
  }

  return {
    sessionId,
    title: session?.title ?? UNTITLED,
    ok: status === "done",
    status,
    report: reportOf(db, messageId, status),
    changedFiles,
  };
}

/** `running` means the row outlived the process that owned it — that is orphaned. */
function finalStatus(turn: Turn | undefined): SubagentResult["status"] {
  const status = turn?.status;
  return status === "done" || status === "error" || status === "interrupted" ? status : "orphaned";
}

/**
 * The child's final text, with a guaranteed non-empty fallback that says WHY.
 *
 * A normally-completing turn always writes prose (the runner's report nudge and
 * forced text round see to it), and error and interrupt each append a note this
 * picks up. The real fallback case is an orphaned turn, which left nothing at all —
 * and "" travelling up to the spawner as a report reads as a child that succeeded
 * silently, which is the one thing it did not do.
 */
function reportOf(db: Db, messageId: string, status: SubagentResult["status"]): string {
  const text = (db.getMessage(messageId)?.parts ?? [])
    .filter((p): p is Extract<typeof p, { type: "text" }> => p.type === "text")
    .map((p) => p.text)
    .join("\n")
    .trim();
  if (text) return text;
  return {
    done: "The subagent finished without writing a report.",
    error: "The subagent errored before reporting.",
    interrupted: "The subagent was interrupted before reporting.",
    orphaned: "The subagent was orphaned (the server restarted) before reporting.",
  }[status];
}

/**
 * Keyword search is maintained on insert (plan T8.9). A failure to index is a
 * degraded search, never a failed launch.
 */
function indexQuietly(db: Db, message: Message): void {
  try {
    db.indexMessage(message);
  } catch (err) {
    console.error(`failed to index subagent task message ${message.id}:`, err);
  }
}
