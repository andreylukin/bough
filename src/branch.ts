/**
 * Shared branch-seeding mechanism for the two "history is a tree" operations that both
 * spin up a new sibling session seeded from an existing one: compaction (compact.ts,
 * span → summary) and fork-at-message (fork.ts, cut a thread at a turn). Both rely on
 * thread-through-parents: a new session parented at the target's parent inherits the
 * shared ancestors for free, and its own seeded messages reconstruct the rest.
 *
 * openBranch creates + announces the new session and hands back a Seeder; the caller
 * adds messages (copies of originals and/or fresh ones) in thread order. Every seeded
 * message is announced as message.started so the UI's existing reducers render it.
 *
 * Ordering: seeded messages use Date.now() and are ordered by (created_at, rowid) —
 * insertion order breaks same-ms ties (see db.messagesFor). We deliberately do NOT
 * advance an artificial clock, so a real turn started afterwards (its message stamped
 * with a later/equal Date.now() and a higher rowid) always sorts after the seed.
 */
import { z } from "zod";
import type { Db } from "./db/db.ts";
import type { Bus } from "./bus.ts";
import type { Message, Part, Session, SessionKind } from "./schema/parts.ts";

/**
 * One picked message for a selection-driven branch op (compact/extract): the whole
 * message, or — when `parts` is set — just those sections of it. Part indexes point
 * into the message's parts array, so the UI can offer "this turn minus its tool
 * calls" and the server stays agnostic about part types.
 */
export const PartPick = z.object({
  messageId: z.string(),
  parts: z.array(z.number().int().nonnegative()).min(1).optional(),
});
export type PartPick = z.infer<typeof PartPick>;

/**
 * Merge duplicate picks by message: a whole-message pick wins over a partial one;
 * partial picks union their indexes (sorted). null = the whole message.
 */
export function mergePicks(picks: PartPick[]): Map<string, number[] | null> {
  const merged = new Map<string, Set<number> | null>();
  for (const p of picks) {
    if (!p.parts) {
      merged.set(p.messageId, null);
      continue;
    }
    const cur = merged.get(p.messageId);
    if (cur === null) continue; // already picked whole
    const set = cur ?? new Set<number>();
    for (const i of p.parts) set.add(i);
    merged.set(p.messageId, set);
  }
  return new Map([...merged].map(([id, s]) => [id, s ? [...s].sort((a, b) => a - b) : null]));
}

/**
 * A message's parts narrowed to the picked indexes (null = all of them), or
 * undefined when an index is out of range — the caller turns that into its 400.
 */
export function pickParts(m: Message, indexes: number[] | null): Part[] | undefined {
  if (indexes === null) return m.parts;
  if (indexes.some((i) => i >= m.parts.length)) return undefined;
  return indexes.map((i) => m.parts[i]);
}

export interface BranchCtx {
  db: Db;
  bus: Bus;
}

export interface BranchSpec {
  parentId: string | null;
  title: string;
  kind: SessionKind;
  /** Inherited onto the branch when set (fork carries the origin's workspace). */
  workspace?: string | null;
  /** Lineage: the session this branch came from (fork source / compacted session). */
  originId?: string | null;
  /** Lineage: the at-message (fork) / span-end message (compaction). */
  originMessageId?: string | null;
}

/** Create the branch session, publish session.created, and return a Seeder for it. */
export function openBranch(ctx: BranchCtx, spec: BranchSpec): Seeder {
  const session: Session = {
    id: crypto.randomUUID(),
    parentId: spec.parentId,
    title: spec.title,
    kind: spec.kind,
    createdAt: Date.now(),
    // Absent when unset, so responses/events stay byte-identical (toSession only
    // surfaces these when non-null).
    ...(spec.workspace ? { workspace: spec.workspace } : {}),
    ...(spec.originId ? { originId: spec.originId } : {}),
    ...(spec.originMessageId ? { originMessageId: spec.originMessageId } : {}),
  };
  ctx.db.createSession(session);
  ctx.bus.publish({ type: "session.created", sessionId: session.id, data: session });
  return new Seeder(ctx, session);
}

/** Appends seeded messages to a freshly opened branch, announcing each. */
export class Seeder {
  constructor(private readonly ctx: BranchCtx, readonly session: Session) {}

  /** Append a message with the given role + parts; announce it; return it. */
  add(role: Message["role"], parts: Part[]): Message {
    const msg: Message = {
      id: crypto.randomUUID(),
      sessionId: this.session.id,
      role,
      parts,
      pending: false,
      createdAt: Date.now(),
    };
    this.ctx.db.createMessage(msg);
    this.ctx.bus.publish({ type: "message.started", sessionId: this.session.id, data: msg });
    return msg;
  }

  /** Append a deep copy of an existing message (new id, same role + content). */
  copy(m: Message): Message {
    return this.add(m.role, JSON.parse(JSON.stringify(m.parts)) as Part[]);
  }
}
