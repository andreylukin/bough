/**
 * Branch seeding — the one mechanism under every history operation that spins up a
 * new session carrying copies of an existing one's turns: fork (T8.2), compaction
 * (T8.3), extract (T8.5), handoff (T8.7), and — via a directly constructed `Seeder`
 * — move-into (T8.6), which appends onto a session that already exists.
 *
 * THE INVARIANT THIS HOLDS: **a seeded message is stamped with the real clock, never
 * with an advanced artificial one** (plan §6.1). Messages order by
 * `(created_at, rowid)` (`db.messagesFor`), so insertion order is what separates two
 * writes that land in the same millisecond — and a branch is *always* followed
 * immediately by something else: fork's "edit & resend" starts a real turn microseconds
 * after the last seeded copy. Stamping the seed with a counter that runs ahead of
 * `Date.now()` (`base + i`, "one ms per message") would put that turn's user message
 * *before* the end of the seed and reorder history under the user; stamping it behind
 * would do the same to the copies. Reading the wall clock once per message and letting
 * `rowid` break the tie is the only version that cannot go wrong in either direction.
 *
 * The clock is nevertheless injected (`ctx.now`, per plan §0's DI rule) — that is what
 * lets `branch.test.ts` pin every write in the whole scenario to one millisecond and
 * prove the tie-break actually carries the ordering. Injected, but never *advanced*:
 * `add()` calls `now()` once and stores exactly what it returned.
 *
 * The second thing this file encodes is why seeding is cheap: **thread-through-parents.**
 * A branch parented at the TARGET'S PARENT inherits every shared ancestor's messages for
 * free (`db.threadFor` = ancestors root→parent, then own), so only the target's own turns
 * are ever copied — however deep the lineage runs (spec §14). Callers pass
 * `parentId: target.parentId`, not `target.id`.
 *
 * The third: **every seeded message is announced as `message.started`.** A branch is not
 * a special client state — it is a session with messages in it — so the UI's existing
 * reducers render a seeded transcript with no changes at all, and a client that missed
 * the events rebuilds the same thing from `GET /sessions/:id` (events are display
 * transport; plan §6.16).
 *
 * Ported from `src/branch.ts`. Deltas from that port are marked `NOTE:`.
 */
import type { Message, Part, Session, SessionKind } from "../schema/parts.ts";
import type { PartPick } from "../schema/requests.ts";
import type { Bus, Db } from "../types.ts";

/**
 * NOTE: `PartPick` is defined once in `schema/requests.ts` (frozen) rather than
 * redeclared here as the old tree did — it is a request shape, and the selection-driven
 * ops parse it at the router edge. Re-exported so a history module needs one import for
 * the pick helpers and the type they take.
 */
export type { PartPick };

// ---------------------------------------------------------------------------
// Part picks — the selection helpers the pick-driven ops share
// ---------------------------------------------------------------------------

/**
 * Merge duplicate picks by message id: a whole-message pick wins over a partial one,
 * and partial picks union their indexes (sorted). `null` = the whole message.
 *
 * Duplicates are not a client bug to reject: a UI that offers both "this turn" and
 * "this section" will send both for the same message the moment a user selects a
 * section and then the turn around it, and the obvious intent is the union.
 */
export function mergePicks(picks: readonly PartPick[]): Map<string, number[] | null> {
  const merged = new Map<string, Set<number> | null>();
  for (const p of picks) {
    if (!p.parts) {
      merged.set(p.messageId, null);
      continue;
    }
    const cur = merged.get(p.messageId);
    if (cur === null) continue; // already picked whole — a partial can't narrow it
    const set = cur ?? new Set<number>();
    for (const i of p.parts) set.add(i);
    merged.set(p.messageId, set);
  }
  return new Map(
    [...merged].map(([id, s]) => [id, s ? [...s].sort((a, b) => a - b) : null]),
  );
}

/**
 * A message's parts narrowed to the picked indexes (`null` = all of them), or
 * `undefined` when an index is out of range — the caller turns that into its own 400.
 *
 * Returning `undefined` rather than throwing keeps this pure and keeps the error text
 * with the operation that has the vocabulary for it (fork says one thing about a bad
 * pick, extract another).
 */
export function pickParts(m: Message, indexes: number[] | null): Part[] | undefined {
  if (indexes === null) return m.parts;
  if (indexes.some((i) => i >= m.parts.length)) return undefined;
  return indexes.map((i) => m.parts[i]);
}

/** One resolved pick: where the message sat in the thread, and the view to copy. */
export interface ResolvedPick {
  /** Index in the thread the picks were resolved against — the sort key. */
  idx: number;
  /** The message narrowed to its picked parts. Never a reference to the original. */
  view: Message;
}

/**
 * Resolve part-picks against a thread: merge duplicates, validate membership and part
 * ranges, and return the picked views **in thread order**.
 *
 * Order is restored here rather than trusted from the request because the client sends
 * a selection, not a sequence — a user shift-clicking upward would otherwise seed a
 * branch with its turns reversed.
 *
 * `err` wraps a message in the caller's error type (`ForkError`, `ExtractError`, …), so
 * one router catch renders it with the right status and this stays free of HTTP.
 */
export function resolvePicks(
  thread: readonly Message[],
  picks: readonly PartPick[],
  err: (message: string) => Error,
): ResolvedPick[] {
  const index = new Map(thread.map((m, i) => [m.id, i]));
  return [...mergePicks(picks)]
    .map(([id, sel]) => {
      const i = index.get(id);
      if (i === undefined) throw err("picks must be messages of the source thread");
      const parts = pickParts(thread[i], sel);
      if (parts === undefined) throw err(`part index out of range for message ${id}`);
      return { idx: i, view: { ...thread[i], parts } };
    })
    .sort((a, b) => a.idx - b.idx);
}

// ---------------------------------------------------------------------------
// Titles
// ---------------------------------------------------------------------------

/**
 * A session title with accumulated branch prefixes stripped.
 *
 * Branching a branch composes titles — fork a fork and you get "fork · fork · X" —
 * which is noise in every picker within two operations. Callers prefix the BASE title
 * instead, so the label always says what the session is, once.
 */
export function baseTitle(title: string): string {
  return title.replace(/^((fork|extract|subagent|handoff) · )+/, "");
}

// ---------------------------------------------------------------------------
// Opening a branch
// ---------------------------------------------------------------------------

/**
 * What seeding needs from the world. Structurally satisfied by `AppCtx` and `TurnCtx`,
 * so a caller passes the ctx it already has; declared as its own (narrower) interface so
 * a test can hand over two objects and nothing else.
 */
export interface BranchCtx {
  db: Db;
  bus: Bus;
  /** Injected clock. Absent = `Date.now`. Never advanced by the seeder — see the header. */
  now?: () => number;
}

export interface BranchSpec {
  /**
   * The TARGET'S PARENT for fork and compaction — that is what makes the branch a
   * sibling that inherits the shared ancestors for free. `null` for a fresh root
   * (extract, handoff).
   */
  parentId: string | null;
  title: string;
  kind: SessionKind;
  /** Inherited when set: a fork works the same checkout, in place (spec §14). */
  workspace?: string | null;
  /** The project dir the lineage is for; inherited, never re-derived (spec §4). */
  originDir?: string | null;
  /**
   * NOTE: not in the old tree, which snapshotted workspaces. Here the Changes rail is
   * `git diff <base>` (spec §13), so a branch that inherits its target's workspace must
   * inherit the sha that workspace's change set is measured from — otherwise the fork
   * shows no changes for work that is plainly in the tree. Absent = no change set, which
   * is the correct answer for a non-git workspace and for a branch with no checkout.
   */
  base?: string | null;
  /** Lineage: the session this branched from (fork source / compacted session). */
  originId?: string | null;
  /** Lineage: the at-message (fork) / last picked message (compaction). */
  originMessageId?: string | null;
}

/**
 * Create the branch session, publish `session.created`, and return a `Seeder` for it.
 *
 * The session is announced *before* any message is seeded, because the events are
 * consumed in order: a `message.started` for a session the client has never heard of is
 * a message it has nowhere to put.
 */
export function openBranch(ctx: BranchCtx, spec: BranchSpec): Seeder {
  const session: Session = {
    id: crypto.randomUUID(),
    parentId: spec.parentId,
    title: spec.title,
    kind: spec.kind,
    createdAt: (ctx.now ?? Date.now)(),
    // Present only when set, so a branch's row and the events describing it carry
    // exactly the lineage it has and nothing that reads as an explicit null.
    ...(spec.workspace ? { workspace: spec.workspace } : {}),
    ...(spec.originDir ? { originDir: spec.originDir } : {}),
    ...(spec.base ? { base: spec.base } : {}),
    ...(spec.originId ? { originId: spec.originId } : {}),
    ...(spec.originMessageId ? { originMessageId: spec.originMessageId } : {}),
  };
  // NOTE: announce what STORAGE kept, not the argument — `createSession` reads the row
  // back, so the event and a later `GET /sessions/:id` cannot disagree.
  const stored = ctx.db.createSession(session);
  ctx.bus.publish({ type: "session.created", sessionId: stored.id, data: stored });
  return new Seeder(ctx, stored);
}

/**
 * Appends seeded messages to a session, announcing each one.
 *
 * Constructed directly (rather than only through `openBranch`) by move-into, which seeds
 * an existing session — the append behaviour is identical, only the session's origin
 * differs.
 */
export class Seeder {
  constructor(private readonly ctx: BranchCtx, readonly session: Session) {}

  /**
   * Append a message with the given role and parts, announce it, and return it as
   * stored.
   *
   * `now()` is read here, once, per message. Nothing derives a timestamp from the
   * previous one: that is the whole ordering invariant (see the header).
   */
  add(role: Message["role"], parts: Part[]): Message {
    const stored = this.ctx.db.createMessage({
      id: crypto.randomUUID(),
      sessionId: this.session.id,
      role,
      parts,
      // A seeded message is history, complete on arrival. `pending` is the supervisor's
      // streaming flag; setting it would leave the branch looking like a turn that never
      // finished, and nothing exists to close it.
      pending: false,
      createdAt: (this.ctx.now ?? Date.now)(),
    });
    // Keyword search is maintained on insert (plan T8.9). A rebuild indexes every row,
    // so skipping this would make the incremental and rebuilt indexes disagree — which
    // is precisely what T8.9's AC forbids.
    indexQuietly(this.ctx.db, stored);
    this.ctx.bus.publish({
      type: "message.started",
      sessionId: this.session.id,
      data: stored,
    });
    return stored;
  }

  /**
   * Append a copy of an existing message: new id, same role, deep-copied parts.
   *
   * The deep copy is not ceremony. The caller usually hands over a message it read from
   * the source session, and a shared `parts` array would let a later edit on either side
   * reach into the other's transcript — history is a tree precisely because nothing is
   * ever rewritten in place (spec §2.4). The round-trip through JSON is deliberate: it
   * produces exactly what storage would hold, and it cannot throw on a stray non-JSON
   * value mid-seed the way a structured clone can.
   */
  copy(m: Message): Message {
    return this.add(m.role, JSON.parse(JSON.stringify(m.parts)) as Part[]);
  }
}

/**
 * A seeded message that fails to index is a degraded search, never a half-seeded branch:
 * the throw would abandon the copies already written with no way to finish them.
 */
function indexQuietly(db: Db, message: Message): void {
  try {
    db.indexMessage(message);
  } catch (err) {
    console.error(`failed to index seeded message ${message.id}:`, err);
  }
}
