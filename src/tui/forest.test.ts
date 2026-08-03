/**
 * The one tree, asserted on fixtures with no terminal.
 *
 * This file inherits from the two it replaces (`Tree.test.ts`'s lineage cases and
 * `historytree.test.ts`'s conversation cases), because the rules did not change when
 * the two surfaces merged — only the list they are expressed in did:
 *
 *   - spec §4, **"visibility is derived, not stored"**: `subagent` and
 *     `workflow_agent` collapse under their origin and surface only on drill-in;
 *     roots and their branches are always listed; there is no archive, deprecate,
 *     hide or purge state anywhere in the model. A regression here does not look
 *     like a crash, it looks like a fan-out burying the conversation it belongs to,
 *     or a branch nobody can reach.
 *   - pi's `/tree` selection rules, which are bough's fork (`selectionFor`).
 *
 * …plus the rules that only exist because there is one tree now: a conversation's
 * turns appear when it is expanded, a branch hangs off the TURN it cut from rather
 * than off its parent session, and the top level stays a usable switcher.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import type { Message, SessionKind } from "../schema/parts.ts";
import type { SessionRow } from "./api.ts";
import {
  forestRows,
  isDelegated,
  messageGist,
  revealPath,
  rewindIndex,
  selectionFor,
  takeBackTarget,
} from "./forest.ts";

let clock = 1_700_000_000_000;

function session(id: string, kind: SessionKind, over: Partial<SessionRow> = {}): SessionRow {
  return { id, title: id, kind, createdAt: clock++, parentId: null, busy: false, ...over };
}

const msg = (id: string, role: Message["role"], text: string): Message =>
  ({
    id,
    role,
    parts: text ? [{ type: "text", text }] : [],
    pending: false,
    createdAt: Number(id.replace(/\D/g, "")) || 1,
  }) as Message;

const THREAD = [
  msg("m1", "user", "add a discount function"),
  msg("m2", "supervisor", "done, it multiplies by (1 - pct/100)"),
  msg("m3", "user", "now validate pct"),
];

/** Row ids, with a marker for the two non-session kinds, so a shape reads at a glance. */
const shape = (rows: ReturnType<typeof forestRows>) =>
  rows.map((r) =>
    r.kind === "session"
      ? r.id
      : r.kind === "message"
      ? `·${r.id}`
      : r.kind === "section"
      ? `§${r.label}`
      : `⋯${r.originId}`
  );

function build(over: Partial<Parameters<typeof forestRows>[0]> = {}) {
  return forestRows({
    sessions: [],
    childrenByOrigin: {},
    threads: {},
    expanded: new Set(),
    drilled: new Set(),
    ...over,
  });
}

/**
 * One root, a fork of it cut at m1, and a fan-out of three workflow agents under the
 * fork — the ordinary shape of a session that branched once and delegated once.
 */
function fixture() {
  const root = session("root", "root");
  const fork = session("fork", "fork", {
    originId: "root",
    parentId: "root",
    originMessageId: "m1",
  });
  const agents = ["w1", "w2", "w3"].map((id) => session(id, "workflow_agent", { originId: "fork" }));
  const sub = session("sub", "subagent", { originId: "root" });
  return {
    sessions: [root, fork],
    childrenByOrigin: { root: [fork, sub], fork: agents },
    threads: { root: THREAD },
  };
}

// ---- lineage (inherited from the session tree) -------------------------------

test("delegated kinds are the ones that collapse", () => {
  assert.equal(isDelegated("subagent"), true);
  assert.equal(isDelegated("workflow_agent"), true);
  assert.equal(isDelegated("root"), false);
  assert.equal(isDelegated("fork"), false);
  assert.equal(isDelegated("compaction"), false);
});

test("a collapsed conversation is ONE row — the top level stays a switcher", () => {
  const rows = build(fixture());
  // Nothing is expanded, so the forest is exactly the conversations you can pick
  // from. A tree that showed every turn of every conversation on arrival would be
  // thousands of rows and useless as the thing you switch with.
  assert.deepEqual(shape(rows), ["root"]);
  assert.equal(rows[0].kind === "session" && rows[0].expandable, true);
});

test("a fan-out collapses to one countable row, and drill-in surfaces just that one", () => {
  const f = fixture();
  const closed = build({ ...f, expanded: new Set(["root", "fork"]) });
  // The fork is listed under the turn it cut from; its three agents are one row
  // saying how many, and the root's subagent likewise. Nothing is hidden outright:
  // a branch the user cannot see is a branch they cannot reach.
  assert.deepEqual(shape(closed), ["root", "·m1", "fork", "⋯fork", "·m2", "·m3", "⋯root"]);

  const drilled = build({ ...f, expanded: new Set(["root", "fork"]), drilled: new Set(["fork"]) });
  assert.deepEqual(
    shape(drilled),
    ["root", "·m1", "fork", "w1", "w2", "w3", "·m2", "·m3", "⋯root"],
  );
  // The root's own fan-out stays collapsed: expanding one origin must not unfold the
  // tree beneath or beside it.
  assert.ok(shape(drilled).includes("⋯root"));
});

test("a fork listed at the top level is drawn once, under the TURN it branched from", () => {
  const f = fixture();
  const rows = build({ ...f, expanded: new Set(["root"]) });
  const forks = rows.filter((r) => r.kind === "session" && r.id === "fork");
  assert.equal(forks.length, 1);
  // Depth 2: root (0) → its turn (1) → the branch off that turn (2). This is the
  // fact the two old tabs could not express between them — lineage knew the parent
  // SESSION and the conversation view knew the turn, and neither showed both.
  assert.equal(forks[0].depth, 2);
  // …and it is NOT also a root, which is what `GET /sessions` would suggest.
  assert.equal(build(f)[0].id, "root");
  assert.equal(build(f).length, 1);
});

test("a branch whose origin turn is not in the thread is still reachable", () => {
  // A compaction can drop the turn a branch cut from. Placing it by `originMessageId`
  // and stopping there would make it invisible — and an unreachable branch is the
  // one outcome spec §4 rules out.
  const root = session("root", "root");
  const orphan = session("orphan", "fork", { originId: "root", originMessageId: "gone" });
  const rows = build({
    sessions: [root, orphan],
    childrenByOrigin: { root: [orphan] },
    threads: { root: THREAD },
    expanded: new Set(["root"]),
  });
  assert.deepEqual(shape(rows), ["root", "·m1", "·m2", "·m3", "orphan"]);
});

test("a lineage cycle renders a short tree instead of hanging the terminal", () => {
  // `originId` is a pointer the server sets for the tree, not a foreign key — a
  // malformed one must not be an infinite walk.
  const a = session("a", "root");
  const b = session("b", "fork", { originId: "a" });
  const rows = build({
    sessions: [a, b],
    childrenByOrigin: { a: [b], b: [a] },
    threads: { a: [], b: [] },
    expanded: new Set(["a", "b"]),
  });
  assert.deepEqual(shape(rows), ["a", "b"]);
});

test("an unfetched thread is not an empty one", () => {
  // `threads[id] === undefined` means "not read yet" and `[]` means "no turns". A
  // conversation in the first state must still offer its disclosure, or the row
  // reads as a leaf and the user stops trying.
  const unfetched = build({ sessions: [session("root", "root")], expanded: new Set() });
  assert.equal(unfetched[0].kind === "session" && unfetched[0].expandable, true);
  const empty = build({
    sessions: [session("root", "root")],
    threads: { root: [] },
  });
  assert.equal(empty[0].kind === "session" && empty[0].expandable, false);
});

test("branches sort by creation, so a row does not move under the cursor", () => {
  const root = session("root", "root");
  const late = session("late", "fork", { originId: "root", createdAt: 2000 });
  const early = session("early", "fork", { originId: "root", createdAt: 1000 });
  const rows = build({
    sessions: [root, late, early],
    childrenByOrigin: { root: [late, early] },
    threads: { root: [] },
    expanded: new Set(["root"]),
  });
  assert.deepEqual(shape(rows), ["root", "early", "late"]);
});

// ---- the top level ----------------------------------------------------------

test("conversations are newest first — this list is also the switcher", () => {
  const old = session("old", "root", { createdAt: 1 });
  const recent = session("recent", "root", { createdAt: 9 });
  assert.deepEqual(shape(build({ sessions: [old, recent] })), ["recent", "old"]);
});

test("the filter narrows the top level and never hides the open conversation", () => {
  const a = session("a", "root", { title: "wire the panel" });
  const b = session("b", "root", { title: "nightly bench" });
  assert.deepEqual(shape(build({ sessions: [a, b], filter: "bench" })), ["b"]);
  // Narrowing the list until the conversation you are IN disappears from it is
  // disorienting, and it is the row the cursor most often wants to get back to.
  assert.deepEqual(
    shape(build({ sessions: [a, b], filter: "bench", currentId: "a" })),
    ["b", "a"],
  );
  assert.equal(build({ sessions: [a, b], currentId: "a" }).some((r) =>
    r.kind === "session" && r.current
  ), true);
});

// ---- turns (inherited from the conversation tree) ---------------------------

test("every turn is a row, and the last one is the active leaf", () => {
  const rows = build({
    sessions: [session("root", "root")],
    threads: { root: THREAD },
    expanded: new Set(["root"]),
  });
  assert.deepEqual(shape(rows), ["root", "·m1", "·m2", "·m3"]);
  const active = rows.filter((r) => r.kind === "message" && r.active);
  assert.equal(active.length, 1);
  assert.equal(active[0].id, "m3");
});

test("user-only is a filter on the rows, not on the leaf", () => {
  const rows = build({
    sessions: [session("root", "root")],
    threads: { root: THREAD },
    expanded: new Set(["root"]),
    userOnly: true,
  });
  assert.deepEqual(shape(rows), ["root", "·m1", "·m3"]);
});

test("a turn with no prose is still a node — it is somewhere you can go back to", () => {
  const calls = {
    id: "m9",
    role: "supervisor",
    pending: false,
    createdAt: 9,
    parts: [
      { type: "tool_call", id: "c1", name: "run_steps", input: {} },
      { type: "tool_call", id: "c2", name: "run_steps", input: {} },
    ],
  } as unknown as Message;
  assert.equal(messageGist(calls), "(2 steps)");
  assert.equal(messageGist({ ...calls, parts: [] } as Message), "(no text)");
});

test("a long gist is truncated with an ellipsis, a short one is left alone", () => {
  assert.equal(messageGist(msg("m1", "user", "short")), "short");
  const long = messageGist(msg("m1", "user", "x".repeat(80)));
  assert.equal(long.length, 56);
  assert.ok(long.endsWith("…"));
});

// ---- what ⏎ means -----------------------------------------------------------

test("Enter follows pi's selection rules, addressed to the row's OWN conversation", () => {
  const f = fixture();
  const rows = build({ ...f, expanded: new Set(["root"]) });
  const threads = f.threads;
  const at = (id: string) => rows.find((r) => r.id === id)!;

  // A USER turn cuts BEFORE itself and hands its text back, so you edit and re-send
  // — pi's "leaf set to parent, message text placed in editor".
  assert.deepEqual(selectionFor(at("m1"), threads), {
    fork: { sessionId: "root", atMessageId: "m1", exclusive: true },
    editorText: "add a discount function",
  });
  // Anything else cuts inclusive with an empty composer.
  assert.deepEqual(selectionFor(at("m2"), threads), {
    fork: { sessionId: "root", atMessageId: "m2" },
  });
  // A conversation OPENS — the switcher half of this surface stays one keypress.
  assert.deepEqual(selectionFor(at("root"), threads), { open: "root" });
  assert.deepEqual(selectionFor(at("fork"), threads), { open: "fork" });
  // A collapsed fan-out drills in.
  assert.deepEqual(selectionFor(at("collapsed:root"), threads), { drill: "root" });
});

// ---- what a take-back acts on ----------------------------------------------

test("the take-back prefers a queued message, then the last user turn", () => {
  // A queued message never reached the server, so it is both the most recent thing
  // said and the one with nothing to undo anywhere else.
  assert.deepEqual(takeBackTarget(["typed while busy"], THREAD), { kind: "queued" });
  // Otherwise it is the last USER turn — m3 here, not m2, and not the whole thread.
  assert.deepEqual(takeBackTarget([], THREAD), {
    kind: "sent",
    atMessageId: "m3",
    text: "now validate pct",
  });
  // The supervisor may already be answering inside the window; the user turn is
  // still the one that was sent.
  assert.deepEqual(
    takeBackTarget([], [...THREAD, msg("m4", "supervisor", "validating…")]),
    { kind: "sent", atMessageId: "m3", text: "now validate pct" },
  );
  // Armed by a send whose message has not reached the thread yet: nothing to do.
  assert.deepEqual(takeBackTarget([], []), { kind: "none" });
  assert.deepEqual(takeBackTarget([], [msg("m1", "supervisor", "hi")]), { kind: "none" });
});

test("a taken-back message comes back verbatim, not as a gist", () => {
  // What the tree shows in a row and what the composer has to receive are different
  // things: a gist collapses whitespace, and getting a three-paragraph message back
  // as one line means rebuilding what you wrote.
  const multiline = msg("m9", "user", "first line\n\n  indented second\n");
  const target = takeBackTarget([], [multiline]);
  assert.equal(target.kind === "sent" && target.text, "first line\n\n  indented second\n");
  assert.equal(messageGist(multiline, Infinity), "first line indented second");
});

// ---- where esc esc lands ----------------------------------------------------

test("rewind lands on the open conversation's last USER turn", () => {
  const f = fixture();
  const rows = build({ ...f, expanded: new Set(["root"]), currentId: "root" });
  // …not the last turn (m3 IS the last user turn here), and never the top of the
  // forest: `esc esc` exists to go back one message, and making that a scroll
  // through every other conversation on the machine is the whole failure.
  assert.equal(rows[rewindIndex(rows, "root")].id, "m3");

  // With the agent having spoken last, it is still the last USER turn that is the
  // one you would re-say.
  const answered = build({
    sessions: [session("root", "root")],
    threads: { root: [...THREAD, msg("m4", "supervisor", "validated")] },
    expanded: new Set(["root"]),
    currentId: "root",
  });
  assert.equal(answered[rewindIndex(answered, "root")].id, "m3");

  // A conversation with no turns yet falls back to its own row, and no conversation
  // at all falls back to the top rather than to nothing.
  const bare = build({ sessions: [session("root", "root")], threads: { root: [] } });
  assert.equal(rewindIndex(bare, "root"), 0);
  assert.equal(rewindIndex(bare, null), 0);
});

/**
 * A row that hides live work must not read as finished. Driving a fan-out and then
 * opening the tree showed `● ✓` on the conversation whose five subagents were, two
 * rows below on the same screen, reported as "5 agents running".
 */
test("running work under a conversation is counted on its row", () => {
  const root = session("root", "root");
  const a = session("a", "subagent", { originId: "root", busy: true });
  const b = session("b", "subagent", { originId: "root" });
  // A branch of a branch: depth must not hide it.
  const fork = session("fork", "fork", { originId: "root" });
  const deep = session("deep", "subagent", { originId: "fork", lastTurnStatus: "running" });

  const rows = forestRows({
    sessions: [root, fork],
    childrenByOrigin: { root: [a, b, fork], fork: [deep] },
    threads: { root: [msg("m1", "user", "go")] },
    expanded: new Set(["root"]),
    drilled: new Set(),
  });
  const rootRow = rows.find((r) => r.kind === "session" && r.id === "root");
  assert.equal(rootRow?.kind === "session" && rootRow.busyBelow, 2);
  // The idle sibling reports nothing, so the count means what it says.
  const forkRow = rows.find((r) => r.kind === "session" && r.id === "fork");
  assert.equal(forkRow?.kind === "session" && forkRow.busyBelow, 1);
});

/**
 * The navigation bug: a handoff, a fork and a compaction all hang under what they came
 * from, so the conversation being typed into was a COLLAPSED row inside another one —
 * the tree showed everything except where you were.
 */
test("revealPath names the origins to expand to reach the current conversation", () => {
  const root = session("root", "root");
  const fork = session("fork", "fork", { originId: "root" });
  const handoff = session("hand", "root", { originId: "fork" });
  const sessions = [root, fork, handoff];

  assert.deepEqual(revealPath(sessions, {}, "hand"), ["root", "fork"]);
  assert.deepEqual(revealPath(sessions, {}, "fork"), ["root"]);
  // A root is already at the top level: nothing to open.
  assert.deepEqual(revealPath(sessions, {}, "root"), []);
  assert.deepEqual(revealPath(sessions, {}, null), []);
  assert.deepEqual(revealPath(sessions, {}, "unknown"), []);
  // Children fetched on drill-in are part of the map too.
  assert.deepEqual(revealPath([root], { root: [fork] }, "fork"), ["root"]);
});

test("revealPath survives a lineage cycle rather than hanging the terminal", () => {
  const x = session("x", "fork", { originId: "y" });
  const y = session("y", "fork", { originId: "x" });
  const path = revealPath([x, y], {}, "x");
  assert.ok(path.length <= 2, path.join(","));
});


/**
 * The keymap has always said `/` in the tree "searches every message", and `^r` is listed
 * as unavailable BECAUSE of it — but the filter only ever compared titles and workspaces.
 * Typing `/compound` answered `nothing matches "compound"` while `GET /search?q=compound`
 * returned five hits in three conversations: endpoint, client method and legend all
 * shipped, and nothing ever called it.
 */
test("a conversation whose MESSAGES match survives the filter", () => {
  const a = session("alpha", "root", { title: "pricing bug" });
  const b = session("beta", "root", { title: "unrelated" });
  const rows = (over: Partial<Parameters<typeof forestRows>[0]> = {}) =>
    forestRows({
      sessions: [a, b],
      childrenByOrigin: {},
      threads: {},
      expanded: new Set(),
      drilled: new Set(),
      filter: "compound",
      ...over,
    }).filter((r) => r.kind === "session").map((r) => r.id);

  // Title-only matching hides both: neither title contains the word.
  assert.deepEqual(rows(), []);
  // With the search's answer, the conversation that said it is reachable.
  assert.deepEqual(rows({ matchedSessions: ["beta"] }), ["beta"]);
  // The title match still works, and the two combine rather than replacing each other.
  assert.deepEqual(
    rows({ filter: "pricing", matchedSessions: ["beta"] }).sort(),
    ["alpha", "beta"],
  );
  // The open conversation is never filtered out, matched or not.
  assert.deepEqual(rows({ currentId: "alpha" }), ["alpha"]);
});

/**
 * `POST /sessions/:id/sections`, its LLM pass and `api.sections` all shipped and nothing called
 * them, so a long conversation expanded into row after row of `you …` / `bough …` with no sign
 * of where the subject changed — on the surface whose whole job is finding a turn again.
 */
test("topic sections caption the turns beneath them", () => {
  const root = session("root", "root");
  const thread = [
    msg("m1", "user", "fix the discount"),
    msg("m2", "supervisor", "fixed"),
    msg("m3", "user", "now the shipping rules"),
    msg("m4", "supervisor", "done"),
  ];
  const rows = forestRows({
    sessions: [root],
    childrenByOrigin: {},
    threads: { root: thread },
    expanded: new Set(["root"]),
    drilled: new Set(),
    sections: {
      root: [
        { start: 0, end: 1, label: "the discount bug" },
        { start: 2, end: 3, label: "shipping rules" },
      ],
    },
  });
  assert.deepEqual(shape(rows), [
    "root",
    "§the discount bug",
    "·m1",
    "·m2",
    "§shipping rules",
    "·m3",
    "·m4",
  ]);
  // A section belongs to its conversation, which is what lets ← close that conversation from it.
  const first = rows.find((r) => r.kind === "section");
  assert.equal(first?.kind === "section" && first.sessionId, "root");

  // A label with no letters is not a topic. The route returns them: a real answer for an
  // 8-turn conversation ended `{"start":7,"end":7,"label":"…"}`.
  assert.deepEqual(
    shape(forestRows({
      sessions: [root],
      childrenByOrigin: {},
      threads: { root: thread },
      expanded: new Set(["root"]),
      drilled: new Set(),
      sections: { root: [{ start: 0, end: 1, label: "…" }, { start: 2, end: 3, label: "shipping" }] },
    })),
    ["root", "·m1", "·m2", "§shipping", "·m3", "·m4"],
  );

  // Absent sections render no headers — "not fetched" is not "no topics".
  assert.deepEqual(
    shape(forestRows({
      sessions: [root],
      childrenByOrigin: {},
      threads: { root: thread },
      expanded: new Set(["root"]),
      drilled: new Set(),
    })),
    ["root", "·m1", "·m2", "·m3", "·m4"],
  );
});

/**
 * `selectionFor` must be TOTAL over the row kinds. Adding the section row without touching it
 * left ⏎ on a caption falling through to the fork branch, where
 * `threads[...].find(x => x.id === row.id)` cannot match a `section:<id>:<i>` id — so the panel
 * would have asked the server to fork at a message that does not exist.
 */
test("⏎ on a topic caption does nothing at all", () => {
  const row = {
    kind: "section" as const,
    id: "section:root:0",
    sessionId: "root",
    depth: 1,
    label: "the discount bug",
  };
  assert.deepEqual(selectionFor(row, { root: [msg("m1", "user", "fix it")] }), { none: true });

  // The other kinds still resolve as before — a caption must not have changed them.
  assert.deepEqual(
    selectionFor({ kind: "collapsed", id: "c", originId: "root", depth: 1, count: 2 }, {}),
    { drill: "root" },
  );
});

/**
 * The `/` filter narrowing to a conversation answers "which one" and leaves the reader to find
 * the turn by eye in forty rows — the job they opened search to avoid. The row that actually
 * said the word is marked.
 */
test("a searched turn is marked, and only that turn", () => {
  const root = session("root", "root", { title: "pricing" });
  const thread = [
    msg("m1", "user", "fix the discount"),
    msg("m2", "supervisor", "the compound bug is in fees_total"),
    msg("m3", "user", "thanks"),
  ];
  const rows = forestRows({
    sessions: [root],
    childrenByOrigin: {},
    threads: { root: thread },
    expanded: new Set(["root"]),
    drilled: new Set(),
    filter: "compound",
    matchedSessions: ["root"],
    matchedMessages: ["m2"],
  });
  const marked = rows.filter((r) => r.kind === "message" && r.matched).map((r) => r.id);
  assert.deepEqual(marked, ["m2"]);
  // Every turn still renders — search marks, it does not filter turns away, because the
  // surrounding turns are the context that makes a hit readable.
  assert.deepEqual(
    rows.filter((r) => r.kind === "message").map((r) => r.id),
    ["m1", "m2", "m3"],
  );

  // With no match list nothing is marked.
  const plain = forestRows({
    sessions: [root],
    childrenByOrigin: {},
    threads: { root: thread },
    expanded: new Set(["root"]),
    drilled: new Set(),
  });
  assert.equal(plain.some((r) => r.kind === "message" && r.matched), false);
});
