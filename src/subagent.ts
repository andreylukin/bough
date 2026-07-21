/**
 * Subagents — delegation as tree branches. A subagent is a real session (kind
 * "subagent") spawned mid-turn by a supervisor program via the delegation host
 * functions: it gets a fresh, task-only thread (parentId null — deliberately NO
 * inherited context; the task text is the whole briefing, though the spawning
 * turn's MCP grant carries over — see TurnCtx.mcpGrant), lineage pointers back to
 * the spawning turn (originId/originMessageId — exactly what the heads map draws
 * connectors from), and its own shadow worktree branched off the spawner's tip so
 * parallel subagents never fight over one working copy.
 *
 * Two delegation modes share one launch path:
 *   - runSubagent (program: `agent(task)`) blocks until the subagent's turn ends and
 *     returns its result in-band. Interrupting the spawner interrupts it.
 *   - spawnSubagentDetached (program: `spawn(task)`) returns the handle immediately
 *     and the subagent runs on regardless of what the spawner does — keep working,
 *     end the turn, whatever. When it finishes, its report is delivered to the
 *     spawner as a system note (postSystemNote), which wakes an idle spawner with a
 *     fresh turn or rides the queued-drain if one is mid-flight. joinSubagent
 *     (program: `join(sessionId)`) claims the result in-band instead of the note.
 *
 * Results never auto-merge: adoptSubagent squashes the subagent's change into the
 * spawner's, or the branch is simply left on the map for the user to review,
 * continue, or archive. Because the subagent is a plain session with a workspace
 * and a bookmark, "continue off of it" needs nothing new: open it and send a message.
 */
import { join as joinPath } from "node:path";
import type { Db } from "./db/db.ts";
import { openBranch } from "./branch.ts";
import {
  beginTurn,
  interruptTurn,
  isTurnRunning,
  onInterrupt,
  postSystemNote,
  type TurnCtx,
} from "./turn.ts";
import { DONE_ACCEPTED } from "./tools/mod.ts";
import { maybeAutoTitle, UNTITLED } from "./supervisor/title.ts";
import { normalizeWorkspace } from "./supervisor/workspace.ts";
import * as shadow from "./vcs/shadow.ts";

export interface SpawnCtx {
  spawnerId: string;
  /** The spawner's in-flight supervisor message — the lineage point on the map. */
  spawnerMessageId: string;
  /** The spawner turn's resolved model; the subagent inherits it. */
  model?: string;
  /** The spawner turn's abort signal; interrupting it interrupts BLOCKING waits. */
  signal?: AbortSignal;
}

export interface SubagentResult {
  sessionId: string;
  title: string;
  /** The subagent's turn ran to completion (no error, not interrupted). */
  ok: boolean;
  /** Why it ended: "done" | "error" | "interrupted" | "orphaned" — so the parent
   * (and the completion note) can say WHY, not just that it failed. */
  status: "done" | "error" | "interrupted" | "orphaned";
  /** The harness accepted `done` (committed CHECK passed). */
  checkPassed: boolean;
  /** The subagent's final message text. */
  report: string;
  /** Paths changed on the subagent's branch (empty without a repo workspace). */
  changedFiles: string[];
}

/** The immediate handle a background spawn returns to the program. */
export interface SubagentHandle {
  sessionId: string;
  title: string;
}

/** Wall-clock cap per subagent turn; overrun interrupts it (result reports ok:false).
 * Env-overridable (BOUGH_SUBAGENT_TIMEOUT_MS) so the timeout path is testable. */
function turnTimeoutMs(): number {
  const n = Number(Deno.env.get("BOUGH_SUBAGENT_TIMEOUT_MS"));
  return Number.isFinite(n) && n > 0 ? n : 15 * 60_000;
}

/**
 * Nesting cap: sessions at depth < MAX may delegate, so a root (0) spawns subagents
 * (1) which spawn nested subagents (2), and depth 2 is terminal. Subagent turns get
 * BLOCKING delegation only (agent/adopt, no spawn/join): a detached child would
 * outlive the turn whose report already went upward, mutating a branch the spawner
 * believes is final.
 */
export const MAX_SUBAGENT_DEPTH = 2;

/** Delegation depth: 0 for a non-subagent session, else 1 + its origin's depth. */
export function subagentDepth(db: Db, sessionId: string): number {
  let depth = 0;
  let cur = db.getSession(sessionId);
  // Lineage is set once at spawn so cycles can't happen; cap hops anyway.
  while (cur?.kind === "subagent" && depth < 16) {
    depth++;
    cur = cur.originId ? db.getSession(cur.originId) : undefined;
  }
  return depth;
}

/**
 * Width caps — the depth cap alone doesn't bound fan-out. Both refuse the spawn
 * with an error the model reads and adapts to; neither interrupts running work.
 *   - MAX_TREE_CONCURRENT: subagent turns running at once across the whole tree.
 *   - MAX_SPAWNS_PER_TURN: total spawns from one spawning turn (message), so a
 *     loop can't fork unbounded even sequentially.
 */
export const MAX_TREE_CONCURRENT = 4;
export const MAX_SPAWNS_PER_TURN = 8;

/** The top session of a subagent tree (a non-subagent, or an orphaned lineage tip). */
function treeRootOf(db: Db, sessionId: string): string {
  let id = sessionId;
  let cur = db.getSession(id);
  for (let hops = 0; cur?.kind === "subagent" && cur.originId && hops < 16; hops++) {
    const origin = db.getSession(cur.originId);
    if (!origin) break;
    id = origin.id;
    cur = origin;
  }
  return id;
}

/** How many subagents in `rootId`'s tree currently have a turn in flight. */
function runningInTree(db: Db, rootId: string): number {
  const all = db.listSessions().filter((s) => s.kind === "subagent");
  let running = 0;
  const frontier = [rootId];
  while (frontier.length) {
    const id = frontier.pop()!;
    for (const s of all) {
      if (s.originId !== id) continue;
      if (isTurnRunning(s.id)) running++;
      frontier.push(s.id);
    }
  }
  return running;
}

/**
 * Background subagents by session id: join() claims the result in-band; otherwise
 * completion posts the report to the spawner as a system note. In-memory like the
 * turn runner's `running` map — a server restart orphans these (recoverOrphanedTurns
 * surfaces the stranded turns).
 */
const detached = new Map<string, {
  spawnerId: string;
  result: Promise<SubagentResult>;
  claimed: boolean;
}>();

/**
 * Spawn-time label: the task's first line, word-truncated to ~40 chars. During a
 * parallel fan-out every card would otherwise read "untitled" until the title
 * worker backfills — the stub makes siblings tellable apart immediately, and it
 * doubles as the placeholder the worker's content-derived name replaces.
 */
export function taskStubTitle(task: string): string {
  const line = task.trim().split("\n")[0]?.replace(/\s+/g, " ").trim() ?? "";
  if (!line) return UNTITLED;
  if (line.length <= 40) return line;
  const cut = line.slice(0, 40);
  const at = cut.lastIndexOf(" ");
  // Cut at the last word boundary unless that throws away most of the budget.
  return `${(at > 20 ? cut.slice(0, at) : cut).trimEnd()}…`;
}

async function pathExists(p: string): Promise<boolean> {
  try {
    await Deno.stat(p);
    return true;
  } catch {
    return false;
  }
}

/**
 * The spawner's explicit workspace, resolved the same way prepareWorkspace does
 * (session column, then BOUGH_WORKSPACE), or undefined when it runs on the bare cwd.
 */
function explicitWorkspace(ctx: TurnCtx, sessionId: string): string | undefined {
  const raw = ctx.db.getSessionRuntime(sessionId).workspace ??
    Deno.env.get("BOUGH_WORKSPACE") ?? undefined;
  return raw === undefined ? undefined : normalizeWorkspace(raw);
}

/** Assemble the result once a subagent's turn has finished. */
async function buildResult(
  db: Db,
  sessionId: string,
  title: string,
  messageId: string,
  subDir: string | undefined,
): Promise<SubagentResult> {
  // The turn's persisted status decides ok; the harness's DONE_ACCEPTED marker in
  // the final message decides checkPassed (deterministic done, not say-so).
  const turn = db.turnForMessage(messageId);
  const parts = db.getMessage(messageId)?.parts ?? [];
  const report = parts
    .filter((p): p is Extract<typeof p, { type: "text" }> => p.type === "text")
    .map((p) => p.text)
    .join("\n")
    .trim();
  const checkPassed = parts.some((p) =>
    p.type === "tool_result" && typeof p.output === "string" && p.output.includes(DONE_ACCEPTED)
  );
  let changedFiles: string[] = [];
  if (subDir) {
    try {
      changedFiles = (await shadow.diff(subDir, sessionId)).files.map((f) => f.path);
    } catch {
      // diff is best-effort reporting; the branch itself is intact
    }
  }
  const status = (turn?.status ?? "orphaned") as SubagentResult["status"];
  return { sessionId, title, ok: status === "done", status, checkPassed, report, changedFiles };
}

/**
 * Shared launch path: create the subagent session (+ its shadow worktree when the
 * spawner has a repo), seed the task, and begin its turn. Returns immediately with
 * a promise for the assembled result; a timeout interrupts overruns in both modes.
 */
async function launch(
  ctx: TurnCtx,
  spawn: SpawnCtx,
  task: string,
): Promise<{ sessionId: string; title: string; result: Promise<SubagentResult> }> {
  const { db, bus } = ctx;
  if (typeof task !== "string" || !task.trim()) {
    throw new Error("agent/spawn(task): task must be a non-empty string");
  }
  const spawner = db.getSession(spawn.spawnerId);
  if (!spawner) throw new Error("spawner session not found");
  // Defense in depth: the turn runner already withholds delegation at the cap.
  if (subagentDepth(db, spawn.spawnerId) >= MAX_SUBAGENT_DEPTH) {
    throw new Error(`subagent depth limit (${MAX_SUBAGENT_DEPTH}) reached`);
  }
  // Width caps: bound concurrency across the tree and total spawns per turn.
  const spawnedThisTurn =
    db.listSessions().filter((s) =>
      s.kind === "subagent" && s.originMessageId === spawn.spawnerMessageId
    ).length;
  if (spawnedThisTurn >= MAX_SPAWNS_PER_TURN) {
    throw new Error(
      `spawn cap reached: this turn already spawned ${MAX_SPAWNS_PER_TURN} subagents — ` +
        `do the remaining work yourself or continue in a later turn`,
    );
  }
  const running = runningInTree(db, treeRootOf(db, spawn.spawnerId));
  if (running >= MAX_TREE_CONCURRENT) {
    throw new Error(
      `subagent concurrency cap reached (${running} running in this tree, max ${MAX_TREE_CONCURRENT}) — ` +
        `wait for or join() running subagents before spawning more`,
    );
  }

  const explicit = explicitWorkspace(ctx, spawn.spawnerId);
  // Mirrors prepareWorkspace's sandbox rule: only an explicit workspace (and no
  // test override / escape hatch) gets snapshot tracking — and only a git repo
  // gets an isolated shadow worktree of its own.
  const sandboxed = ctx.workspace === undefined && explicit !== undefined &&
    Deno.env.get("BOUGH_NO_SANDBOX") !== "1";
  // A shadow worktree carries a .git FILE; legacy jj-era dirs a .jj dir. Accept
  // both so a nested spawn never silently runs unisolated on its spawner's copy
  // (legacy dirs then fail the spawn with a clear error instead).
  const isRepo = sandboxed &&
    (await pathExists(joinPath(explicit!, ".git")) ||
      await pathExists(joinPath(explicit!, ".jj")));

  const stub = taskStubTitle(task);
  const seeder = openBranch({ db, bus }, {
    parentId: null, // fresh context: the task text is the subagent's whole briefing
    // Immediate task-derived stub, so parallel spawns never all read "untitled";
    // the title worker names the branch properly from the task below.
    title: stub,
    kind: "subagent",
    workspace: explicit ?? null,
    originId: spawn.spawnerId,
    originMessageId: spawn.spawnerMessageId,
  });
  const session = seeder.session;

  // A repo workspace gets its own working copy, branched off the spawner's tip.
  let subDir: string | undefined;
  if (isRepo) {
    try {
      const dir = shadow.workspaceDirFor(session.id);
      await Deno.mkdir(shadow.workspacesRoot(), { recursive: true });
      await shadow.addWorkspace(explicit!, session.id, dir, spawn.spawnerId);
      db.setSessionWorkspace(session.id, dir);
      const updated = db.getSession(session.id)!;
      bus.publish({ type: "session.updated", sessionId: session.id, data: updated });
      subDir = dir;
    } catch (e) {
      // Without an isolated working copy the subagent would race the spawner's
      // checkout — fail the spawn instead of running unisolated.
      db.archiveSession(session.id);
      bus.publish({
        type: "session.archived",
        sessionId: session.id,
        data: { sessionId: session.id },
      });
      throw new Error(`could not branch a workspace for the subagent: ${(e as Error).message}`);
    }
  }

  seeder.add("user", [{ type: "text", text: task }]);
  // Fire-and-forget: name the branch from its task (same worker path as sessions);
  // the stub is the placeholder it replaces.
  maybeAutoTitle({ db, bus, titler: ctx.titler }, session.id, task, stub);
  const { message, done } = beginTurn({ ...ctx, model: spawn.model ?? ctx.model }, session.id);

  const timer = setTimeout(() => interruptTurn(session.id), turnTimeoutMs());
  const result = done
    .finally(() => clearTimeout(timer))
    // Re-read the title at completion — the title worker has usually named it by then.
    .then(() =>
      buildResult(db, session.id, db.getSession(session.id)?.title ?? UNTITLED, message.id, subDir)
    );
  return { sessionId: session.id, title: db.getSession(session.id)?.title ?? UNTITLED, result };
}

/**
 * Blocking delegation (program: `agent(task)`): spawn and wait for the result.
 * Interrupting the spawner's turn interrupts the subagent too — it is part of
 * this turn's work, unlike a detached spawn.
 */
export async function runSubagent(
  ctx: TurnCtx,
  spawn: SpawnCtx,
  task: string,
): Promise<SubagentResult> {
  if (spawn.signal?.aborted) throw new Error("turn interrupted");
  const h = await launch(ctx, spawn, task);
  const onAbort = () => interruptTurn(h.sessionId);
  spawn.signal?.addEventListener("abort", onAbort, { once: true });
  try {
    return await h.result;
  } finally {
    spawn.signal?.removeEventListener("abort", onAbort);
  }
}

/**
 * Background delegation (program: `spawn(task)`): returns the handle immediately.
 * The subagent is NOT tied to the spawner's turn — it survives the turn ending and
 * even an interrupt. On completion the report is posted to the spawner as a system
 * note (waking it if idle), unless a join() claimed the result first.
 */
export async function spawnSubagentDetached(
  ctx: TurnCtx,
  spawn: SpawnCtx,
  task: string,
): Promise<SubagentHandle> {
  if (spawn.signal?.aborted) throw new Error("turn interrupted");
  const h = await launch(ctx, spawn, task);
  const entry = { spawnerId: spawn.spawnerId, result: h.result, claimed: false };
  detached.set(h.sessionId, entry);
  // An explicit interrupt of the spawner cascades to this detached child — the
  // only stop path for a runaway. Unregistered when it finishes on its own.
  const unhook = onInterrupt(spawn.spawnerId, () => interruptTurn(h.sessionId));
  h.result
    .then((r) => {
      if (entry.claimed) return;
      postSystemNote(ctx, spawn.spawnerId, formatNote(r));
    })
    .catch((err) => console.error(`detached subagent ${h.sessionId} failed:`, err))
    .finally(() => unhook());
  return { sessionId: h.sessionId, title: h.title };
}

/**
 * Claim a background subagent's result in-band (program: `join(sessionId)`) instead
 * of the completion note. While waiting, interrupting the spawner's turn releases
 * the wait by interrupting the subagent — same containment as the blocking mode.
 */
export async function joinSubagent(
  spawn: SpawnCtx,
  subagentId: string,
): Promise<SubagentResult> {
  const entry = detached.get(subagentId);
  if (!entry || entry.spawnerId !== spawn.spawnerId) {
    throw new Error("join(sessionId): not a background subagent of this session");
  }
  entry.claimed = true;
  const onAbort = () => interruptTurn(subagentId);
  spawn.signal?.addEventListener("abort", onAbort, { once: true });
  try {
    return await entry.result;
  } finally {
    spawn.signal?.removeEventListener("abort", onAbort);
  }
}

/** The completion note a detached subagent posts back to its spawner. Says WHY it
 * ended so the parent can react (retry an error, respect a stop, re-run a timeout). */
function formatNote(r: SubagentResult): string {
  const status = r.status === "done"
    ? (r.checkPassed ? "finished, check passed" : "finished (no check committed)")
    : r.status === "error"
    ? "FAILED — its turn errored (see the report for the error)"
    : r.status === "interrupted"
    ? "STOPPED — interrupted (a user stop, or it hit the time limit)"
    : "ORPHANED — the server restarted before it finished";
  const files = r.changedFiles.length ? r.changedFiles.join(", ") : "none";
  return [
    `[subagent finished] "${r.title}" (${r.sessionId}) — ${status}.`,
    `Changed files on its branch: ${files}.`,
    r.report ? `Report:\n${r.report}` : "No report.",
    `Its changes stay on its own branch — adopt("${r.sessionId}") in run_steps merges them into this workspace; or leave the branch for review.`,
  ].join("\n");
}

/**
 * Adopt a finished subagent's changes: fold its diff into the spawner's worktree.
 * The subagent's branch stays on the map (emptied, still continuable).
 */
export async function adoptSubagent(
  ctx: TurnCtx,
  spawnerId: string,
  subagentId: string,
): Promise<string> {
  const { db, bus } = ctx;
  const sub = db.getSession(subagentId);
  if (!sub || sub.kind !== "subagent" || sub.originId !== spawnerId) {
    throw new Error("adopt(sessionId): not a subagent of this session");
  }
  const subDir = db.getSessionRuntime(subagentId).workspace;
  const repo = explicitWorkspace(ctx, spawnerId);
  if (!subDir || !repo || subDir === repo) {
    throw new Error("this subagent has no branched workspace to adopt");
  }
  await shadow.adoptChanges(repo, subDir, subagentId, spawnerId);
  // Both Changes rails move: the spawner gains the diff, the subagent's empties.
  bus.publish({ type: "changes.updated", sessionId: spawnerId, data: { sessionId: spawnerId } });
  bus.publish({ type: "changes.updated", sessionId: subagentId, data: { sessionId: subagentId } });
  return `adopted: subagent ${subagentId}'s changes are now in this session's workspace`;
}
