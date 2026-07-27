/**
 * Tests for the session tree.
 *
 * All of them are about one sentence in spec §4: **"Visibility is derived, not
 * stored."** Sessions of kind `subagent` and `workflow_agent` collapse under their
 * `originId` and surface only on drill-in; roots and their branches are always listed;
 * and there is no archive, deprecate, hide or purge state anywhere in the model. So the
 * assertions below are about what `treeItems` puts in the list given nothing but
 * `kind`, `originId` and one set of expanded ids — and, just as load-bearing, what it
 * leaves out. A regression here does not look like a crash; it looks like a fan-out
 * quietly burying the conversation it belongs to, or a branch the user cannot reach.
 *
 * The last test pins the absence: no fixture carries a deprecated flag because no such
 * field exists, and the rendered rows must never grow one.
 */
import assert from "node:assert/strict";
import type { SessionKind } from "../../schema/parts.ts";
import type { SessionRow } from "../api.ts";
import { isDelegated, statusMark, titleOf, Tree, treeItems } from "./Tree.tsx";

let clock = 1_700_000_000_000;

function session(
  id: string,
  kind: SessionKind,
  over: Partial<SessionRow> = {},
): SessionRow {
  return {
    id,
    title: id,
    kind,
    createdAt: clock++,
    parentId: null,
    busy: false,
    ...over,
  };
}

/**
 * One root, a fork of it, and a fan-out of three workflow agents under the fork —
 * the ordinary shape of a session that delegated once.
 */
function fixture() {
  const root = session("root", "root");
  const fork = session("fork", "fork", { originId: "root", parentId: "root" });
  const agents = ["w1", "w2", "w3"].map((id) =>
    session(id, "workflow_agent", { originId: "fork" })
  );
  const sub = session("sub", "subagent", { originId: "root" });
  return {
    roots: [root, fork],
    childrenByOrigin: { root: [fork, sub], fork: agents },
  };
}

Deno.test("delegated kinds are the ones that collapse", () => {
  assert.equal(isDelegated("subagent"), true);
  assert.equal(isDelegated("workflow_agent"), true);
  assert.equal(isDelegated("root"), false);
  assert.equal(isDelegated("fork"), false);
  assert.equal(isDelegated("compaction"), false);
});

Deno.test("a fan-out collapses to one countable row under its origin", () => {
  const { roots, childrenByOrigin } = fixture();
  const items = treeItems({ roots, childrenByOrigin, expanded: new Set() });

  const ids = items.map((i) => i.type === "session" ? i.session.id : `⋯${i.originId}`);
  // The fork is listed (a branch, always visible); its three workflow agents are one
  // row saying how many, and the root's subagent likewise. Nothing is hidden outright:
  // a branch the user cannot see is a branch they cannot reach.
  assert.deepEqual(ids, ["root", "fork", "⋯fork", "⋯root"]);

  const collapsed = items.filter((i) => i.type === "collapsed");
  assert.deepEqual(collapsed.map((c) => c.type === "collapsed" && c.count), [3, 1]);
});

Deno.test("drill-in surfaces exactly the origin that was expanded", () => {
  const { roots, childrenByOrigin } = fixture();
  const items = treeItems({ roots, childrenByOrigin, expanded: new Set(["fork"]) });

  const ids = items.map((i) => i.type === "session" ? i.session.id : `⋯${i.originId}`);
  assert.deepEqual(ids, ["root", "fork", "w1", "w2", "w3", "⋯root"]);
  // The root's own fan-out stays collapsed: expanding one origin must not unfold the
  // tree beneath or beside it.
  assert.ok(ids.includes("⋯root"));
});

Deno.test("delegated grandchildren stay collapsed until their own parent is opened", () => {
  const roots = [session("root", "root")];
  const childrenByOrigin = {
    root: [session("sub", "subagent", { originId: "root" })],
    sub: [session("nested", "subagent", { originId: "sub" })],
  };
  const shallow = treeItems({ roots, childrenByOrigin, expanded: new Set(["root"]) });
  assert.deepEqual(
    shallow.map((i) => i.type === "session" ? i.session.id : `⋯${i.originId}`),
    ["root", "sub", "⋯sub"],
  );

  const deep = treeItems({ roots, childrenByOrigin, expanded: new Set(["root", "sub"]) });
  assert.deepEqual(
    deep.map((i) => i.type === "session" ? i.session.id : `⋯${i.originId}`),
    ["root", "sub", "nested"],
  );
});

Deno.test("a fork listed at the top level is drawn once, under what it branched from", () => {
  const { roots, childrenByOrigin } = fixture();
  const items = treeItems({ roots, childrenByOrigin, expanded: new Set() });
  const forks = items.filter((i) => i.type === "session" && i.session.id === "fork");
  assert.equal(forks.length, 1);
  assert.equal(forks[0].type === "session" && forks[0].depth, 1);
});

Deno.test("a session carries the count of what is delegated under it, open or not", () => {
  const { roots, childrenByOrigin } = fixture();
  const closed = treeItems({ roots, childrenByOrigin, expanded: new Set() });
  const fork = closed.find((i) => i.type === "session" && i.session.id === "fork");
  assert.equal(fork?.type === "session" && fork.delegated, 3);
  assert.equal(fork?.type === "session" && fork.open, false);

  const open = treeItems({ roots, childrenByOrigin, expanded: new Set(["fork"]) });
  const opened = open.find((i) => i.type === "session" && i.session.id === "fork");
  assert.equal(opened?.type === "session" && opened.open, true);
});

Deno.test("a lineage cycle renders a short tree instead of hanging the terminal", () => {
  // `originId` is a pointer the server sets for the tree, not a foreign key — a
  // malformed one must not be an infinite walk.
  const a = session("a", "root");
  const b = session("b", "fork", { originId: "a" });
  const items = treeItems({
    roots: [a],
    childrenByOrigin: { a: [b], b: [a] },
    expanded: new Set(),
  });
  assert.deepEqual(items.map((i) => i.type === "session" && i.session.id), ["a", "b"]);
});

Deno.test("an unfetched drill-in is an empty fan-out, not a crash", () => {
  const items = treeItems({
    roots: [session("root", "root")],
    childrenByOrigin: {},
    expanded: new Set(["root"]),
  });
  assert.equal(items.length, 1);
});

Deno.test("branches sort by creation, so a row does not move under the cursor", () => {
  const root = session("root", "root");
  const late = session("late", "fork", { originId: "root", createdAt: 2000 });
  const early = session("early", "fork", { originId: "root", createdAt: 1000 });
  const items = treeItems({
    roots: [root],
    childrenByOrigin: { root: [late, early] },
    expanded: new Set(),
  });
  assert.deepEqual(
    items.map((i) => i.type === "session" && i.session.id),
    ["root", "early", "late"],
  );
});

// ---- outcome markers ---------------------------------------------------------

Deno.test("a session that never ran a turn gets no marker", () => {
  assert.equal(statusMark(session("x", "root")), null);
});

Deno.test("a failed delegation is marked even when its turn ended cleanly", () => {
  const failed = session("x", "subagent", { lastTurnStatus: "done", outcomeOk: false });
  assert.deepEqual(statusMark(failed), { glyph: "✗", color: "red" });
  const ok = session("y", "subagent", { lastTurnStatus: "done", outcomeOk: true });
  assert.equal(statusMark(ok)?.glyph, "✓");
});

Deno.test("a restart-orphaned branch is distinguishable from a failed one", () => {
  assert.equal(statusMark(session("x", "fork", { lastTurnStatus: "orphaned" }))?.glyph, "◼");
  assert.equal(statusMark(session("y", "fork", { lastTurnStatus: "error" }))?.glyph, "✗");
  assert.equal(statusMark(session("z", "fork", { busy: true }))?.glyph, "⋯");
});

Deno.test("titles drop the kind prefix the server stamped on them", () => {
  assert.equal(
    titleOf(session("x", "subagent", { title: "subagent · review app.ts" })),
    "review app.ts",
  );
  assert.equal(titleOf(session("y", "root", { title: "" })), "(untitled)");
});

// ---- the absence ------------------------------------------------------------

Deno.test("there is no archive or deprecate affordance to render", () => {
  // Spec §17: "No archive, deprecate, or purge. Visibility is derived from lineage."
  // The component's props are the item list, a cursor and a height — there is nowhere
  // for a `showDeprecated` toggle to enter, and the source carries no such string.
  const source = Deno.readTextFileSync(new URL("./Tree.tsx", import.meta.url));
  // The module comment is where the drop is explained and is expected to name it; the
  // CODE is what must not have grown a field, a prop or a key binding for it.
  const code = source.slice(source.indexOf("*/") + 2).toLowerCase();
  for (const gone of ["deprecat", "archiv", "hidden", "purge"]) {
    assert.ok(!code.includes(gone), `"${gone}" appears in Tree.tsx below the module comment`);
  }
  assert.equal(typeof Tree, "function");
});
