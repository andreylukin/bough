/**
 * The conversation as a tree — pi's `/tree`, on bough's model.
 *
 * WHAT THIS IS FOR. A long session is not a timeline, it is a set of attempts:
 * exploratory dead ends, a hypothesis that did not hold, one turn you wish you had
 * phrased differently. pi (`badlogic/pi-mono`, `docs/tree.md`) makes that concrete
 * by treating a session as a tree with a movable leaf, and it is the feature this
 * module ports: see every turn, see what branched off which turn, and go back to
 * any of them.
 *
 * THE ONE DIFFERENCE FROM pi, stated plainly. pi moves a `leaf` pointer inside one
 * session file. bough has no leaf: every branch is its own session, parented at the
 * message it cut from (spec §14 — "all operate by branching, never by mutating
 * history in place"). So "go back to turn 4 and try again" is a FORK here and a
 * pointer move there. The user-visible behaviour is deliberately identical —
 * pi's own `exclusive` case, "cut before the message because the caller intends to
 * re-send it itself", is exactly what bough's fork body already accepts — and the
 * tree shows the resulting branches in place, so it reads as one tree either way.
 * What bough does not get from this is pi's branch-summary-on-switch; that needs
 * the leaf, and the leaf is a schema change.
 *
 * PURE. Rows in, rows out, no fetch and no ink — so the whole layout is asserted
 * against fixtures, which is the property `lines.ts` and `keys.ts` also hold and
 * the reason the bugs in this repo have been in the parts that lacked it.
 */
import type { Message } from "../schema/parts.ts";
import type { SessionRow } from "./api.ts";

/** One rendered row of the tree. */
export interface TreeRow {
  /** `message` — a turn in this thread. `branch` — a session that cut from one. */
  kind: "message" | "branch";
  /** The message id, or the branch's session id. What `Enter` acts on. */
  id: string;
  /** Tree connectors plus the label, already assembled. */
  text: string;
  /** The thread's last message: pi's `← active`. */
  active: boolean;
  /** Which role, for colouring. Absent on a branch row. */
  role?: Message["role"];
  /** A branch row's session, so the caller can open it without a second lookup. */
  session?: SessionRow;
}

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

export interface HistoryTreeInput {
  /** The open session's thread, ancestors first (`GET /sessions/:id`). */
  thread: readonly Message[];
  /** Sessions that cut from a message of this thread, keyed by `originMessageId`. */
  branches: readonly SessionRow[];
  /** Show only user turns — pi's `Ctrl+U`. */
  userOnly?: boolean;
}

/**
 * Build the rows.
 *
 * The active marker goes on the LAST message rather than on a stored pointer,
 * because in bough the end of the thread IS where the next turn appends — that is
 * what a leaf is, expressed as a consequence of the data rather than as a field.
 */
export function historyTreeRows(input: HistoryTreeInput): TreeRow[] {
  const byOrigin = new Map<string, SessionRow[]>();
  for (const b of input.branches) {
    const at = b.originMessageId;
    if (!at) continue;
    const list = byOrigin.get(at) ?? [];
    list.push(b);
    byOrigin.set(at, list);
  }

  const shown = input.userOnly ? input.thread.filter((m) => m.role === "user") : input.thread;
  const lastId = input.thread.at(-1)?.id;
  const rows: TreeRow[] = [];

  shown.forEach((m, i) => {
    const last = i === shown.length - 1;
    const branches = (byOrigin.get(m.id) ?? []).slice().sort((a, b) => a.createdAt - b.createdAt);
    const active = m.id === lastId;
    rows.push({
      kind: "message",
      id: m.id,
      role: m.role,
      active,
      text: `${last && branches.length === 0 ? "└─" : "├─"} ${roleLabel(m.role)} ${messageGist(m)}${
        active ? "  ← active" : ""
      }`,
    });
    // Branches hang off the turn they cut from, indented, oldest first — the shape
    // pi draws, and the reason the tree answers "what else did I try here".
    branches.forEach((b, j) => {
      rows.push({
        kind: "branch",
        id: b.id,
        session: b,
        active: false,
        text: `│  ${j === branches.length - 1 ? "└─" : "├─"} ⑂ ${b.title || "untitled"}${
          b.busy ? "  ⋯ working" : ""
        }`,
      });
    });
  });
  return rows;
}

/** `supervisor` is the agent — the transcript calls it "bough" and so does this. */
function roleLabel(role: Message["role"]): string {
  return role === "user" ? "you" : role === "supervisor" ? "bough" : role;
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
 */
export function selectionFor(
  row: TreeRow,
  thread: readonly Message[],
): { open: string } | { fork: { atMessageId: string; exclusive?: boolean }; editorText?: string } {
  if (row.kind === "branch") return { open: row.id };
  const m = thread.find((x) => x.id === row.id);
  if (m?.role === "user") {
    return { fork: { atMessageId: row.id, exclusive: true }, editorText: messageGist(m, Infinity) };
  }
  return { fork: { atMessageId: row.id } };
}
