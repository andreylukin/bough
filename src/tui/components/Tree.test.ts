/**
 * The tree's PRESENTATION: the outcome marker, the title, and the absence.
 *
 * What rows exist and in what order is `forest.ts`'s business and is asserted in
 * `forest.test.ts` — this file covers only what this module decides.
 *
 * The last test pins the absence: no fixture carries a deprecated flag because no
 * such field exists, and the rendered rows must never grow one.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import type { SessionKind } from "../../schema/parts.ts";
import type { SessionRow } from "../api.ts";
import type { ForestRow } from "../forest.ts";
import { kindGlyph, markLegend, statusMark, titleOf, Tree } from "./Tree.tsx";

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

// ---- outcome markers ---------------------------------------------------------

test("a session that never ran a turn gets no marker", () => {
  assert.equal(statusMark(session("x", "root")), null);
});

test("a failed delegation is marked even when its turn ended cleanly", () => {
  const failed = session("x", "subagent", { lastTurnStatus: "done", outcomeOk: false });
  assert.deepEqual(statusMark(failed), { glyph: "✗", color: "red" });
  const ok = session("y", "subagent", { lastTurnStatus: "done", outcomeOk: true });
  assert.equal(statusMark(ok)?.glyph, "✓");
});

test("a restart-orphaned branch is distinguishable from a failed one", () => {
  assert.equal(statusMark(session("x", "fork", { lastTurnStatus: "orphaned" }))?.glyph, "◼");
  assert.equal(statusMark(session("y", "fork", { lastTurnStatus: "error" }))?.glyph, "✗");
  assert.equal(statusMark(session("z", "fork", { busy: true }))?.glyph, "⋯");
});

test("titles drop the kind prefix the server stamped on them", () => {
  assert.equal(
    titleOf(session("x", "subagent", { title: "subagent · review app.ts" })),
    "review app.ts",
  );
  // The WORKSPACE before "(untitled)": `worker/titles.ts` refuses an uninformative title
  // and documents this fallback, which existed only in the header. The tree is the
  // switcher — a directory name beats nothing.
  assert.equal(
    titleOf(session("y", "root", { title: "", workspace: "/Users/a/repos/bough" })),
    "bough",
  );
  assert.equal(titleOf(session("y2", "root", { title: "", workspace: "/tmp/proj/" })), "proj");
  // Nothing to fall back to at all is the only case that still reads "(untitled)".
  assert.equal(titleOf(session("y3", "root", { title: "" })), "(untitled)");
  // `handoff` was missing from the list, and a handoff of a still-untitled conversation
  // is titled `handoff · ` server-side — the row rendered as a prefix with nothing
  // after it.
  assert.equal(titleOf(session("z", "root", { title: "handoff · " })), "(untitled)");
  assert.equal(
    titleOf(session("w", "root", { title: "handoff · fix the parser" })),
    "fix the parser",
  );
});

test("live work under a collapsed conversation is glyphed, not hidden", () => {
  // The row reported its own last turn and nothing else, so a conversation sitting on
  // five running subagents rendered `✓` while the rail said "5 agents running".
  const idle = session("x", "root", { lastTurnStatus: "done" });
  assert.equal(statusMark(idle)?.glyph, "✓");
  assert.deepEqual(statusMark(idle, 5), { glyph: "⋯", color: "cyan" });
  // Its own busy state still wins the same way — the two are the same statement.
  assert.equal(statusMark(session("y", "root", { busy: true }), 0)?.glyph, "⋯");
});

// ---- the absence ------------------------------------------------------------

test("there is no archive or deprecate affordance to render", () => {
  // Spec §17: "No archive, deprecate, or purge. Visibility is derived from lineage."
  // The component's props are the item list, a cursor and a height — there is nowhere
  // for a `showDeprecated` toggle to enter, and the source carries no such string.
  const source = readFileSync(new URL("./Tree.tsx", import.meta.url), "utf8");
  // The module comment is where the drop is explained and is expected to name it; the
  // CODE is what must not have grown a field, a prop or a key binding for it.
  const code = source.slice(source.indexOf("*/") + 2).toLowerCase();
  for (const gone of ["deprecat", "archiv", "hidden", "purge"]) {
    assert.ok(!code.includes(gone), `"${gone}" appears in Tree.tsx below the module comment`);
  }
  assert.equal(typeof Tree, "function");
});


/**
 * A handoff is a `root` with an `originId`, and `titleOf` strips its `handoff · ` prefix —
 * so a handoff and the conversation it distilled rendered as the same glyph AND the same
 * words, one nested under the other, with nothing on either row saying which was which.
 */
test("a root that came from another conversation is marked as derived", () => {
  assert.equal(kindGlyph(session("a", "root")), "●");
  assert.equal(kindGlyph(session("b", "root", { originId: "a" })), "↦");
  // The mark distinguishes; it does not replace the kinds that already have their own.
  assert.equal(kindGlyph(session("c", "fork", { originId: "a" })), "⑂");
  assert.equal(kindGlyph(session("d", "compaction", { originId: "a" })), "≣");
  assert.equal(kindGlyph(session("e", "subagent", { originId: "a" })), "◆");
});

// ---------------------------------------------------------------------------
// The mark legend
// ---------------------------------------------------------------------------

/** A `session` forest row, the only kind that carries a mark. */
function forestSession(s: SessionRow, busyBelow = 0): ForestRow {
  return {
    kind: "session",
    id: s.id,
    session: s,
    depth: 0,
    open: false,
    delegated: 0,
    current: false,
    busyBelow,
    expandable: false,
  };
}

test("the mark legend explains what is on screen, and only that", () => {
  // The glyphs were documented in exactly one place — the `?` overlay, fifty rows
  // deep — which is not where anybody is when they are looking at a `⑂` and
  // wondering what it is.
  const legend = markLegend([
    forestSession(session("a", "root", { lastTurnStatus: "done" })),
    forestSession(session("b", "fork", { lastTurnStatus: "error" })),
  ]);
  assert.deepEqual(legend, ["● yours", "⑂ fork", "✓ done", "✗ failed"]);
  // Absent kinds are not described: a static legend would spend scarce columns on
  // `≣ compaction` for everyone who has never made one, and `legendLine` would then
  // drop the entries that ARE on screen.
  assert.equal(legend.some((l) => l.includes("compaction")), false);
  assert.equal(legend.some((l) => l.includes("subagent")), false);
});

test("a derived root reads as a handoff, not as one you started", () => {
  // `kindGlyph`'s own rule, surfaced: kind is still `root`, but `originId` means it
  // came from somewhere and the legend must not call it "yours".
  const legend = markLegend([forestSession(session("h", "root", { originId: "x" }))]);
  assert.deepEqual(legend, ["↦ handoff"]);
});

test("rows that carry no mark contribute nothing to the legend", () => {
  // Section captions and turn rows are rows for window math and nothing else.
  assert.deepEqual(
    markLegend([{ kind: "section", id: "s", sessionId: "a", depth: 0, label: "topic" }]),
    [],
  );
});

test("work running below a collapsed row is legended as running", () => {
  // The `busyBelow` rule the tree already paints: a root sitting on live subagents
  // is running, and the legend has to agree with the glyph beside it.
  assert.deepEqual(
    markLegend([forestSession(session("r", "root", { lastTurnStatus: "done" }), 3)]),
    ["● yours", "⋯ running"],
  );
});
