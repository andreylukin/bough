/**
 * Session CRUD, thread assembly, and the message intake.
 *
 * The invariant this module holds is **derived visibility** (spec §4, §17). A
 * session of kind `subagent` or `workflow_agent` collapses under its `originId`
 * and surfaces only on drill-in — and it does so because of what it *is*, not
 * because anything marked it. There is no archive, deprecate, hide or purge verb
 * here and no column behind one: `GET /sessions` filters on `kind`, and
 * `GET /sessions?originId=` is the drill-in that reveals what collapsed. Two
 * consequences worth stating, because both were bugs in the old tree:
 *
 *   - Every hidden session MUST be reachable through some origin. That is why
 *     `POST /sessions` refuses to create a `subagent`/`workflow_agent`: the
 *     creation body carries no `originId`, so an HTTP-created one would be
 *     invisible to every listing at once. Delegated sessions are created by
 *     `agent()`/`spawn()` (M4), which set the lineage edge.
 *   - Nothing here stores the answer. A client that wants the collapsed view
 *     computes it from `kind` + `originId`, the same way this does.
 *
 * Second invariant: **the thread is assembled, never stored.** `GET /sessions/:id`
 * returns `{session, thread}` where the thread is ancestors root→parent plus the
 * session's own messages (`db.threadFor`). That is what makes a fork parented at
 * its target's parent inherit shared history for free (spec §14), and it is why a
 * reconnecting client re-fetches this endpoint rather than replaying events —
 * `seq` is a dedupe key, not a resume cursor (plan §6.16).
 *
 * Third, and the reason `startTurn` arrives on the ctx rather than as an import:
 * this module persists and announces a user message, and **does not know how a
 * turn runs**. The turn runner is M2. Until it is wired, a post persists the
 * message and emits `message.started`; with a runner present the same handler
 * hands off to it. A message that lands while a turn is already running is
 * persisted and left for the queue drain (T2.4) rather than racing the live turn
 * or being dropped (spec §5).
 */
import { homedir } from "node:os";
import { resolve } from "node:path";
import { BadRequestError, NotFoundError } from "../errors.ts";
import type { Message, Part, Session, SessionKind, TurnStatus } from "../schema/parts.ts";
import {
  CreateSessionBody,
  PatchSessionBody,
  PostMessageBody,
  SetDraftBody,
} from "../schema/requests.ts";
import type { AppCtx } from "../types.ts";
// T8.5. `vcs/` is below `server/` and imports nothing from it, so this adds no cycle
// — deliberately not imported from `server/changes.ts`, which does import `app.ts`.
import { recordBase } from "../vcs/repodiff.ts";
import { type Handler, json, parseBody } from "./http.ts";

// ---- derived visibility ------------------------------------------------------

/**
 * The kinds that collapse under their origin. Delegated work is not a separate
 * concept from a session — a subagent IS a session (spec §7) — so the top-level
 * listing would otherwise fill with machine-spawned branches nobody opened.
 */
export const COLLAPSED_KINDS: readonly SessionKind[] = ["subagent", "workflow_agent"];

/** True when the session surfaces only on drill-in under its `originId`. */
export function isCollapsed(session: Session): boolean {
  return COLLAPSED_KINDS.includes(session.kind);
}

/**
 * A listed session plus the facts the sidebar needs at a glance, all DERIVED at
 * read time from turns and usage rows. None of them is a column on `sessions`:
 * `busy` would be stale the moment a server died mid-turn, and a stored cost would
 * be a second source of truth for something the usage rows already answer.
 */
export interface SessionListItem extends Session {
  /** A turn is in flight. The UI keeps it live from events after this read. */
  busy: boolean;
  /** How the session's most recent turn ended — absent if it has never run one. */
  lastTurnStatus?: TurnStatus;
  /** This session's own spend, omitted when zero so untouched rows stay small. */
  costUsd?: number;
}

/**
 * Decorate a listing. Reads `busySessionIds`/`latestTurnStatuses` once for the
 * whole page rather than per row — the same answer, one query instead of N.
 */
function decorate(ctx: AppCtx, sessions: Session[]): SessionListItem[] {
  const busy = ctx.db.busySessionIds();
  const statuses = ctx.db.latestTurnStatuses();
  return sessions.map((s) => {
    const status = statuses.get(s.id);
    const { costUsd } = ctx.db.sessionUsage(s.id);
    return {
      ...s,
      busy: busy.has(s.id),
      ...(status ? { lastTurnStatus: status } : {}),
      ...(costUsd > 0 ? { costUsd } : {}),
    };
  });
}

// ---- turn hand-off -----------------------------------------------------------

/**
 * How a persisted user message becomes a running turn. M2 owns the
 * implementation; this module owns only the call site.
 *
 * It is read off the ctx instead of imported so that `server/` never depends on
 * `turn/` — and so a test can assert exactly when a turn is started (and, more
 * importantly, when it is NOT: a post into a busy session must queue). It returns
 * `void`, never a promise the handler awaits: the response is a 202 and the turn
 * outlives it.
 */
export type TurnStarter = (ctx: AppCtx, session: Session, message: Message) => unknown;

/** The optional ctx field. Declared here because `AppCtx` (T-1) is frozen. */
export interface WithTurnStarter {
  startTurn?: TurnStarter;
}

function turnStarterOf(ctx: AppCtx): TurnStarter | undefined {
  return (ctx as AppCtx & WithTurnStarter).startTurn;
}

function nowOf(ctx: AppCtx): number {
  return (ctx.now ?? Date.now)();
}

/** 404 with a message naming the id, so a client's log says which one was wrong. */
function requireSession(ctx: AppCtx, id: string): Session {
  const session = ctx.db.getSession(id);
  if (!session) throw new NotFoundError(`session ${id} not found`);
  return session;
}

// ---- workspace ---------------------------------------------------------------

/**
 * Expand `~` and make the path absolute. Kept pure — `home` is a parameter — so
 * the expansion is testable without touching the real one.
 */
export function normalizeWorkspace(raw: string, home: string): string {
  const trimmed = raw.trim();
  const expanded = trimmed === "~"
    ? home
    : trimmed.startsWith("~/")
    ? resolve(home, trimmed.slice(2))
    : trimmed;
  // `resolve` makes a relative path absolute against the server's cwd, which is
  // the only interpretation available — the client and the server share a machine.
  return resolve(expanded);
}

/**
 * Why this rejects at creation rather than letting the session exist: a
 * nonexistent checkout otherwise surfaces one turn later as a shell failure
 * inside the program, which reads as the agent being broken. One clear message
 * here beats a confusing one there.
 */
async function requireDirectory(path: string): Promise<void> {
  let stat: Deno.FileInfo;
  try {
    stat = await Deno.stat(path);
  } catch {
    throw new BadRequestError(`workspace does not exist: ${path}`);
  }
  if (!stat.isDirectory) throw new BadRequestError(`workspace is not a directory: ${path}`);
}

// ---- handlers ----------------------------------------------------------------

/**
 * `GET /sessions` — the top level, with `subagent` and `workflow_agent` excluded.
 * `GET /sessions?originId=<id>` — the drill-in: everything that branched from that
 * session, collapsed kinds included, in creation order.
 *
 * The filter is not "the hidden ones under this origin" but "everything with this
 * origin": a fork and a subagent of the same turn belong to the same drill-in, and
 * splitting them would mean the tree view had to ask twice.
 */
export const listSessions: Handler = (req, ctx) => {
  const originId = new URL(req.url).searchParams.get("originId");
  if (originId !== null) {
    // A typo'd id answering `[]` reads as "nothing branched from it", which is a
    // different fact and sends the caller looking for the wrong bug.
    requireSession(ctx, originId);
    return json(decorate(ctx, ctx.db.sessionsByOrigin(originId)));
  }
  return json(decorate(ctx, ctx.db.listSessions().filter((s) => !isCollapsed(s))));
};

/**
 * `POST /sessions` — a user-facing session: a root, or a fork of an existing one.
 *
 * `kind` defaults from `parentId` because that is the only pair that is ever
 * consistent: a parented session inherits a thread, which is what makes it a
 * branch rather than a root.
 */
export const createSession: Handler = async (req, ctx) => {
  const body = await parseBody(req, CreateSessionBody);
  const kind: SessionKind = body.kind ?? (body.parentId ? "fork" : "root");

  // Derived visibility, enforced at the door: these kinds are reachable only
  // through an `originId` the creation body cannot carry, so one made here would
  // be invisible in every listing. `agent()`/`spawn()` (M4) create them with their
  // lineage edge set.
  if (COLLAPSED_KINDS.includes(kind)) {
    throw new BadRequestError(
      `kind '${kind}' is created by agent()/spawn(), not over HTTP — it needs an origin to collapse under`,
    );
  }

  const parentId = body.parentId ?? null;
  if (parentId && !ctx.db.getSession(parentId)) {
    throw new BadRequestError(`parent ${parentId} not found`);
  }

  let workspace: string | undefined;
  if (body.workspace) {
    workspace = normalizeWorkspace(body.workspace, homedir());
    await requireDirectory(workspace);
  }

  const session: Session = {
    id: crypto.randomUUID(),
    // Untitled until the cheap tier names it from the first message (spec §12).
    // Empty string rather than a placeholder sentinel: the client decides how an
    // unnamed session reads, and no code has to know the sentinel.
    title: body.title ?? "",
    kind,
    createdAt: nowOf(ctx),
    parentId,
    // `originDir` mirrors `workspace` at creation and is never rewritten — it stays
    // the stable record of WHICH project this session is for, even if the
    // workspace moves (spec §4).
    ...(workspace ? { workspace, originDir: workspace } : {}),
  };
  ctx.db.createSession(session);

  // Pins are separate writes, not create columns. Applying them before the
  // announce is what makes the event and the response carry the same session the
  // database holds.
  if (body.model) ctx.db.setSessionModel(session.id, body.model);
  if (body.effort) ctx.db.setSessionEffort(session.id, body.effort);

  // T8.5 — the sha this session starts from, which is the whole of the Changes
  // rail's state (spec §13: the working tree is the tip, `base` is where the
  // session began, and `git diff <base>` plus untracked files is the change set).
  //
  // Recorded HERE, at creation, rather than on the first turn: everything that runs
  // in the workspace — a program, a subagent, a workflow agent, a schedule firing —
  // moves the tree, so a base captured any later than this attributes work already
  // done to the commit it started from and hides it from review.
  //
  // Only for an EXPLICIT workspace. A session that named none runs in the server's
  // own directory (`turn/runner.ts`), and recording that repository's HEAD would
  // give the session a change set full of somebody else's uncommitted work — with a
  // revert button on it.
  //
  // Best-effort by construction (`vcs/repodiff.ts`): a non-repo workspace stores
  // nothing and the rail reports "not a repository" rather than an empty diff, and a
  // git failure costs the diff, never the session.
  if (workspace) await recordBase(ctx.db, session.id, workspace);

  const stored = ctx.db.getSession(session.id)!;
  ctx.bus.publish({ type: "session.created", sessionId: stored.id, data: stored });
  return json(stored, 201);
};

/**
 * `GET /sessions/:id` — `{session, thread, usage}`.
 *
 * This is the reconnect path (spec §3): a client that dropped its SSE connection
 * re-fetches here and reconciles by message id. Everything it needs must therefore
 * be here, which is why usage rides along rather than living on a second endpoint
 * the client would have to fetch in lockstep. `tree` is the session plus every
 * branch collapsed under it — what the status bar shows for delegated work.
 */
export const getSession: Handler = (_req, ctx, params) => {
  const session = requireSession(ctx, params.id);
  return json({
    session,
    thread: ctx.db.threadFor(session.id),
    usage: { ...ctx.db.sessionUsage(session.id), tree: ctx.db.treeUsage(session.id) },
  });
};

/**
 * `POST /sessions/:id/messages` — persist the user message, announce it, and hand
 * off to the turn runner.
 *
 * 202, not 200: the turn outlives this response and reports over `/events`. The
 * body carries the stored message so a client can reconcile it against the
 * `message.started` event by id without a second fetch — which matters for the CLI,
 * where a fast turn can finish before the post returns (plan §6.10).
 */
export const postMessage: Handler = async (req, ctx, params) => {
  const session = requireSession(ctx, params.id);
  const body = await parseBody(req, PostMessageBody);

  const text = body.text.trim();
  const images = body.images ?? [];
  if (!text && images.length === 0) {
    throw new BadRequestError("empty message: text or at least one image is required");
  }

  // A handoff draft is consumed by the first post: whatever the user actually sent
  // supersedes it, edited or not. Announced, unlike the draft PUT below, because
  // here the client is not the one that changed it.
  if (session.draft != null) {
    ctx.db.setSessionDraft(session.id, null);
    const updated = ctx.db.getSession(session.id);
    if (updated) ctx.bus.publish({ type: "session.updated", sessionId: session.id, data: updated });
  }

  // Image bytes never enter the parts JSON — the part carries the path the caller
  // already copied under ~/.bough/attachments (spec §4).
  const parts: Part[] = [
    ...(text ? [{ type: "text", text } as Part] : []),
    ...images.map((i): Part => ({ type: "image", ...i })),
  ];
  const stored = ctx.db.createMessage({
    id: crypto.randomUUID(),
    sessionId: session.id,
    role: "user",
    parts,
    // A user message is complete when it lands; `pending` is the supervisor's
    // streaming flag, and setting it here would leave the transcript looking like
    // a turn that never finished.
    pending: false,
    createdAt: nowOf(ctx),
  });
  // Keyword search is maintained on insert (plan T8.9); idempotent, so a rebuild
  // and this path agree.
  ctx.db.indexMessage(stored);
  ctx.bus.publish({ type: "message.started", sessionId: session.id, data: stored });

  // One turn per session (spec §5). A message that lands mid-turn is persisted and
  // announced like any other, then drains into a fresh turn when the running one
  // ends (T2.4) — it is never dropped and never races the live turn.
  const queued = ctx.db.busySessionIds().has(session.id);
  if (!queued) {
    const start = turnStarterOf(ctx);
    // Fire and forget: a turn runs for minutes and this response is a 202. The
    // catch is not politeness — an unhandled rejection here would take the process
    // down and lose every other session with it.
    if (start) {
      try {
        const running = start(ctx, session, stored);
        if (running instanceof Promise) running.catch(reportTurnStartFailure);
      } catch (e) {
        reportTurnStartFailure(e);
      }
    }
  }

  return json({ message: stored, queued }, 202);
};

function reportTurnStartFailure(error: unknown): void {
  console.error("failed to start turn:", error);
}

/**
 * `PUT /sessions/:id/draft` — the prefilled composer text (spec §4, set by handoff;
 * also where the TUI stashes a half-typed prompt on session switch). `null` clears.
 *
 * **No `session.updated` event, deliberately.** The writer is the client that is
 * switching away; announcing its own write back to it would race the prefill it is
 * about to render and can blank a composer the user is typing into. The post above
 * announces the clear because there the change is not the client's own.
 */
export const putDraft: Handler = async (req, ctx, params) => {
  requireSession(ctx, params.id);
  const body = await parseBody(req, SetDraftBody);
  ctx.db.setSessionDraft(params.id, body.draft);
  return json({ ok: true, draft: body.draft });
};

/**
 * `PATCH /sessions/:id` — the per-session `model` and `effort` overrides (spec §4).
 *
 * These columns have existed since the schema was frozen and nothing wrote them, so
 * the model picker could pin a model in the client and lose it on the next launch.
 * A session's pin is the whole point of the field: switching models mid-conversation
 * must not move every other session with it.
 *
 * Absent field = leave alone; explicit `null` = clear the override and fall back to
 * the global default. The two are deliberately different, because "don't touch this"
 * and "there should be no pin here" are different requests and a picker needs both.
 */
export const patchSession: Handler = async (req, ctx, params) => {
  requireSession(ctx, params.id);
  const body = await parseBody(req, PatchSessionBody);
  if (body.model !== undefined) ctx.db.setSessionModel(params.id, body.model);
  if (body.effort !== undefined) ctx.db.setSessionEffort(params.id, body.effort);
  const session = ctx.db.getSession(params.id)!;
  ctx.bus.publish({ type: "session.updated", sessionId: session.id, data: session });
  return json(session);
};
