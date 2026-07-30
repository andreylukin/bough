/**
 * ONE tree, for everything: conversations, the turns inside them, and what
 * branched off which turn.
 *
 * WHAT THIS REPLACES, AND WHY. There used to be two surfaces and neither one was
 * the whole picture. The `sessions` tab was a flat, recency-ordered list — it knew
 * every conversation and nothing about what was inside one. The `tree` tab was
 * BIMODAL: with a conversation open it drew that conversation's turns, and with
 * none open it drew session lineage instead, so the same key produced two unrelated
 * screens depending on state the user was not thinking about. Neither could answer
 * the ordinary question, "which turn of which conversation did that branch come
 * from" — the answer was split across two tabs whose rows did not correspond.
 *
 * So there is one forest now, and one walk:
 *
 *     ▾ wire the panel                       $0.42
 *       ├─ you   make the rail one row
 *       ├─ bough I found the layout bug…
 *       ├─ you   now name the jobs         ← active
 *       │  └─ ⑂ try it without the ¶
 *       └─ ⋯ 3 delegated
 *     ▸ nightly bench                        $1.08
 *
 * THE RULES, all three of them derived and none of them stored:
 *
 *   1. **A conversation appears exactly once, under what it branched from.** A
 *      fork hangs off the MESSAGE it cut from, which is the fact that was
 *      unreachable before: lineage said "this forked from that session" and the
 *      turn was the part you actually wanted. A session with no origin is a root
 *      and sits at the top level.
 *   2. **Turns are shown for expanded conversations only.** A forest that expanded
 *      everything would be thousands of rows on any real install, and the top level
 *      is also the switcher — it has to stay scannable.
 *   3. **Delegated work still collapses into a count** (spec §4). A 40-agent
 *      fan-out inlined under the turn that spawned it buries the conversation; a
 *      fan-out that is hidden outright is unreachable. One row that says `⋯ 40
 *      delegated` is both. This is the one rule carried over verbatim from the
 *      lineage tree, because it was right.
 *
 * PURE. Rows in, rows out — no fetch, no clock, no React. The caller supplies the
 * threads it has fetched and the sets of what is expanded; every layout rule below
 * is asserted against fixtures, which is the property the two modules this replaces
 * also held and the reason it survives the merge.
 */
import type { Message } from "../schema/parts.ts";
import type { SessionKind } from "../schema/parts.ts";
import type { SessionRow } from "./api.ts";

/** The kinds that collapse under their origin and surface on drill-in (spec §4). */
export const DELEGATED_KINDS: readonly SessionKind[] = ["subagent", "workflow_agent"];

export function isDelegated(kind: SessionKind): boolean {
  return DELEGATED_KINDS.includes(kind);
}

/**
 * One rendered row.
 *
 * `depth` is the indent and `id` is what a keypress acts on — unique across kinds
 * by construction, since a message id and a session id never collide.
 */
export type ForestRow =
  | {
    kind: "session";
    id: string;
    session: SessionRow;
    depth: number;
    /** Its turns are shown. */
    open: boolean;
    /** Delegated children, shown or not — the count the collapsed row reports. */
    delegated: number;
    /** This is the conversation currently on screen. */
    current: boolean;
    /**
     * Conversations BELOW this one that are running right now — delegated or branched,
     * at any depth.
     *
     * A collapsed row was reporting its own last turn and nothing else, so a root
     * whose whole point was the five subagents still working under it rendered
     * `● ✓ done`: the tree said finished while the rail two rows down said "5 agents
     * running". A row that hides live work has to say so.
     */
    busyBelow: number;
    /** It has turns to show (or might — an unfetched thread is not "empty"). */
    expandable: boolean;
  }
  /**
   * A TOPIC HEADER over the turns beneath it, from `POST /sessions/:id/sections`.
   *
   * The route, its LLM pass and `api.sections` all shipped and nothing ever called them, so a
   * long conversation's turn list was forty rows of `you …` / `bough …` with no way to see
   * where the subject changed. Not selectable in any meaningful sense — it is a caption on the
   * rows below it — but it IS a row, so the window math counts it like any other.
   */
  | { kind: "section"; id: string; sessionId: string; depth: number; label: string }
  | {
    kind: "message";
    id: string;
    /** Which conversation this turn belongs to — a fork needs the owner, not the row. */
    sessionId: string;
    depth: number;
    role: Message["role"];
    gist: string;
    /** The thread's last message: where the next turn would append. */
    active: boolean;
    /** Drawn with `└─` rather than `├─`. */
    last: boolean;
  }
  /** The collapsed fan-out: reachable, countable, one row. */
  | { kind: "collapsed"; id: string; originId: string; depth: number; count: number };

/** The visible text of a message, collapsed to one line. */
export function messageGist(m: Message, max = 56): string {
  const text = m.parts
    .map((p) => (p.type === "text" ? p.text : ""))
    .join(" ")
    .replace(/\s+/g, " ")
    .trim();
  if (!text) {
    // A turn that is only tool calls still needs a name — it is a real node, and
    // the whole point of the tree is that you can go back to it.
    const calls = m.parts.filter((p) => p.type === "tool_call").length;
    return calls ? `(${calls} step${calls === 1 ? "" : "s"})` : "(no text)";
  }
  return text.length > max ? `${text.slice(0, max - 1).trimEnd()}…` : text;
}

export interface ForestInput {
  /** `GET /sessions` — every non-delegated session: roots, forks, compactions. */
  sessions: readonly SessionRow[];
  /** `originId` → `GET /sessions?originId=`. Absent means "not fetched yet". */
  childrenByOrigin: Readonly<Record<string, readonly SessionRow[]>>;
  /**
   * Session id → its thread. The open conversation's comes from the store (it is
   * live); any other is fetched when it is expanded. Absent = not fetched, which is
   * NOT the same as empty and must not render as "no turns".
   */
  threads: Readonly<Record<string, readonly Message[]>>;
  /** Sessions whose turns are shown. */
  expanded: ReadonlySet<string>;
  /** Sessions whose delegated fan-out is drilled into. */
  drilled: ReadonlySet<string>;
  /**
   * Topic sections per session, as index ranges over that session's OWN turns — exactly what
   * `POST /sessions/:id/sections` returns for the gists the caller sent. Absent = not fetched,
   * which renders as no headers rather than as "no topics".
   */
  sections?: Readonly<Record<string, readonly { start: number; end: number; label: string }[]>>;
  /** The conversation on screen, marked and never filtered out. */
  currentId?: string | null;
  /** Narrows the TOP LEVEL by title. A branch is never hidden from its parent. */
  filter?: string;
  /**
   * Session ids whose MESSAGES matched the filter, from `GET /search`.
   *
   * The keymap says `/` in the tree "searches every message"; `matches` below only ever
   * compared titles and workspaces, so a term that appears in five messages and no title
   * answered "nothing matches". A row named here survives the filter even when its title
   * does not contain the query.
   */
  matchedSessions?: readonly string[];
  /** Show only user turns — pi's `Ctrl+U`. */
  userOnly?: boolean;
}

/** Newest first at the top level: this is also the switcher, and recency is the order. */
const byNewest = (a: SessionRow, b: SessionRow) => b.createdAt - a.createdAt;
const byOldest = (a: SessionRow, b: SessionRow) => a.createdAt - b.createdAt;

/**
 * Build the rows, depth-first.
 *
 * `seen` is a cycle guard, not a dedupe: `originId` is a pointer the server sets
 * and not a foreign key, so a malformed lineage must render a short tree rather
 * than hang the terminal in an infinite walk.
 */
export function forestRows(input: ForestInput): ForestRow[] {
  const { sessions, childrenByOrigin, threads, expanded, drilled } = input;
  const rows: ForestRow[] = [];
  const seen = new Set<string>();

  /** Every known child of `id`, from both sources, deduped by id. */
  const childrenOf = (id: string): SessionRow[] => {
    const byId = new Map<string, SessionRow>();
    for (const s of sessions) if (s.originId === id) byId.set(s.id, s);
    for (const s of childrenByOrigin[id] ?? []) byId.set(s.id, s);
    return [...byId.values()];
  };

  /** Running descendants at any depth. Cycle-guarded like the walk itself. */
  const busyBelow = (id: string, guard = new Set<string>()): number => {
    if (guard.has(id)) return 0;
    guard.add(id);
    let n = 0;
    for (const c of childrenOf(id)) {
      if (c.busy || c.lastTurnStatus === "running") n++;
      n += busyBelow(c.id, guard);
    }
    return n;
  };

  const walk = (session: SessionRow, depth: number): void => {
    if (seen.has(session.id)) return;
    seen.add(session.id);
    const children = childrenOf(session.id);
    const branches = children.filter((c) => !isDelegated(c.kind)).sort(byOldest);
    const delegated = children.filter((c) => isDelegated(c.kind)).sort(byOldest);
    const thread = threads[session.id];
    const open = expanded.has(session.id);
    rows.push({
      kind: "session",
      id: session.id,
      session,
      depth,
      open,
      delegated: delegated.length,
      current: session.id === input.currentId,
      busyBelow: busyBelow(session.id),
      // A conversation with a fetched-and-empty thread and no branches has nothing
      // under it; one whose thread has not been fetched MIGHT, and rendering it as
      // a leaf would be a claim the caller has not made yet.
      expandable: thread === undefined || thread.length > 0 || branches.length > 0 ||
        delegated.length > 0,
    });
    if (!open) return;

    // The turns, with whatever branched from each one hanging under it. A branch is
    // placed by `originMessageId`; one whose origin turn is not in this thread (a
    // compaction dropped it, or the branch cut from an ancestor) still has to be
    // reachable, so it falls through to the tail below rather than vanishing.
    const shown = input.userOnly ? (thread ?? []).filter((m) => m.role === "user") : thread ?? [];
    const lastId = thread?.at(-1)?.id;
    const placed = new Set<string>();
    const sections = input.sections?.[session.id] ?? [];
    shown.forEach((m, i) => {
      // A label with no letters is not a topic. The route really does return them — a real
      // answer for an 8-turn conversation ended `{"start":7,"end":7,"label":"…"}` — and a header
      // reading `── …` is worse than no header, the same reason `sanitizeTitle` has a floor.
      const head = sections.find((sec) => sec.start === i && /[a-z]/i.test(sec.label));
      if (head) {
        rows.push({
          kind: "section",
          id: `section:${session.id}:${i}`,
          sessionId: session.id,
          depth: depth + 1,
          label: head.label,
        });
      }
      const under = branches.filter((b) => b.originMessageId === m.id);
      rows.push({
        kind: "message",
        id: m.id,
        sessionId: session.id,
        depth: depth + 1,
        role: m.role,
        gist: messageGist(m),
        active: m.id === lastId,
        last: i === shown.length - 1 && under.length === 0,
      });
      for (const b of under) {
        placed.add(b.id);
        walk(b, depth + 2);
      }
    });
    for (const b of branches) if (!placed.has(b.id)) walk(b, depth + 1);

    if (delegated.length === 0) return;
    if (drilled.has(session.id)) {
      for (const child of delegated) walk(child, depth + 1);
    } else {
      rows.push({
        kind: "collapsed",
        id: `collapsed:${session.id}`,
        originId: session.id,
        depth: depth + 1,
        count: delegated.length,
      });
    }
  };

  const roots = sessions
    .filter((s) => !s.originId || !sessions.some((o) => o.id === s.originId))
    .filter((s) => matches(s, input.filter, input.currentId, input.matchedSessions))
    .sort(byNewest);
  for (const root of roots) walk(root, 0);
  return rows;
}

/**
 * Does this top-level conversation survive the filter?
 *
 * The open one always does. Narrowing the list you are looking at until the
 * conversation you are IN disappears from it is disorienting in a way no filter
 * should be, and it is the row the cursor most often wants to return to.
 */
function matches(
  s: SessionRow,
  filter: string | undefined,
  currentId?: string | null,
  matched?: readonly string[],
): boolean {
  const q = (filter ?? "").trim().toLowerCase();
  if (!q) return true;
  if (s.id === currentId) return true;
  if (matched?.includes(s.id)) return true;
  return `${s.title} ${s.workspace ?? ""}`.toLowerCase().includes(q);
}


/**
 * The conversations that must be EXPANDED for `currentId` to be on screen — its
 * chain of origins, nearest last, excluding itself.
 *
 * Opening the tree used to show you everything except where you were. A handoff, a
 * fork and a compaction all hang under the conversation they came from, so the
 * session you were typing into was a collapsed row deep inside another one: the tree
 * offered `← active` on the PARENT's last turn and no hint that the row you wanted
 * was two disclosures away. Seeding the expansion with this puts the cursor's target
 * on screen the moment the panel opens, and because it only SEEDS, a row the user
 * then collapses stays collapsed.
 *
 * Pure and cycle-guarded: `originId` is a pointer the server sets, not a foreign key.
 */
export function revealPath(
  sessions: readonly SessionRow[],
  childrenByOrigin: Readonly<Record<string, readonly SessionRow[]>>,
  currentId: string | null | undefined,
): string[] {
  if (!currentId) return [];
  const byId = new Map<string, SessionRow>();
  for (const s of sessions) byId.set(s.id, s);
  for (const list of Object.values(childrenByOrigin)) for (const s of list) byId.set(s.id, s);
  const path: string[] = [];
  const seen = new Set<string>([currentId]);
  let cur = byId.get(currentId)?.originId ?? null;
  while (cur && !seen.has(cur)) {
    seen.add(cur);
    path.unshift(cur);
    cur = byId.get(cur)?.originId ?? null;
  }
  return path;
}

/** The row a cursor at `selected` is on, or null past the end. */
export function rowAt(rows: readonly ForestRow[], selected: number): ForestRow | null {
  return rows[selected] ?? null;
}

/**
 * The index of the row a rewind should land on: the open conversation's last USER
 * turn, falling back to its last turn and then to its own row.
 *
 * This is what `esc esc` aims at. Landing on the top of the forest would make the
 * common case — "go back one message and say it differently" — a scroll through
 * every other conversation on the machine first.
 */
export function rewindIndex(rows: readonly ForestRow[], currentId: string | null): number {
  if (!currentId) return 0;
  let session = -1;
  let lastTurn = -1;
  let lastUser = -1;
  rows.forEach((r, i) => {
    if (r.kind === "session" && r.id === currentId) session = i;
    if (r.kind === "message" && r.sessionId === currentId) {
      lastTurn = i;
      if (r.role === "user") lastUser = i;
    }
  });
  return lastUser >= 0 ? lastUser : lastTurn >= 0 ? lastTurn : session >= 0 ? session : 0;
}

/**
 * What `Enter` on a row means — pi's selection rules, and bough's fork body.
 *
 * A USER turn cuts BEFORE itself and hands its text back for the composer, so you
 * edit and re-send and that re-send is the new branch. That is pi's "leaf set to
 * parent, message text placed in editor", and it is bough's `exclusive: true`,
 * documented in `schema/requests.ts` as "the caller intends to re-send it itself".
 * Anything else cuts INCLUSIVE and leaves the composer empty: pi's "leaf set to
 * selected node, editor stays empty".
 *
 * THE ONE DIFFERENCE FROM pi, stated plainly. pi moves a `leaf` pointer inside one
 * session file. bough has no leaf: every branch is its own session, parented at the
 * message it cut from (spec §14 — "all operate by branching, never by mutating
 * history in place"). So "go back to turn 4 and try again" is a FORK here and a
 * pointer move there; the user-visible behaviour is deliberately identical.
 */
export type Selection =
  /** Nothing to do — a caption row. Present so `selectionFor` is total over the row kinds. */
  | { none: true }
  | { open: string }
  | { expand: string }
  | { drill: string }
  | {
    fork: { sessionId: string; atMessageId: string; exclusive?: boolean };
    editorText?: string;
  };

export function selectionFor(
  row: ForestRow,
  threads: Readonly<Record<string, readonly Message[]>>,
): Selection {
  if (row.kind === "collapsed") return { drill: row.originId };
  // A SECTION HEADER IS NOT A TURN. Without this it fell through to the fork branch below,
  // where `threads[...].find(x => x.id === row.id)` cannot match a `section:<id>:<i>` row id —
  // so ⏎ on a caption would have asked the server to fork at a message that does not exist.
  // Caught by reading `selectionFor` after adding the row kind, not by a test.
  if (row.kind === "section") return { none: true };
  // ⏎ on a conversation OPENS it — that is what the row is, and it is what the
  // sessions tab's ⏎ always did. Walking into its turns is `→`/`expand`, so the
  // switcher half of this surface stays one keypress.
  if (row.kind === "session") return { open: row.id };
  const m = threads[row.sessionId]?.find((x) => x.id === row.id);
  if (m?.role === "user") {
    return {
      fork: { sessionId: row.sessionId, atMessageId: row.id, exclusive: true },
      editorText: messageGist(m, Infinity),
    };
  }
  return { fork: { sessionId: row.sessionId, atMessageId: row.id } };
}
