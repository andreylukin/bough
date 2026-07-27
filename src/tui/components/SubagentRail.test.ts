/**
 * Tests for the subagent rail.
 *
 * One rule, stated three ways: **the rail pins LIVE subagents only.** A finished branch
 * belongs to the tree and to its report note, both of which outlive the run; a rail that
 * keeps everything it ever saw grows past the terminal on any real fan-out and pushes
 * the composer off screen — which is how the two agents actually working become the part
 * you cannot see. The old tree shipped that bug and fixed it in commit `0b56e12`; these
 * tests are what stop it coming back.
 *
 * `liveSubagents` is the whole rule, so it is tested directly rather than through a
 * render: it is a pure filter over the drill-in children of the open session.
 */
import assert from "node:assert/strict";
import type { SessionKind } from "../../schema/parts.ts";
import type { SessionRow } from "../api.ts";
import { liveSubagents, railHint, railLabel, SubagentRail } from "./SubagentRail.tsx";

let clock = 1_700_000_000_000;

function session(
  id: string,
  kind: SessionKind,
  over: Partial<SessionRow> = {},
): SessionRow {
  return { id, title: id, kind, createdAt: clock++, parentId: null, busy: false, ...over };
}

Deno.test("only running delegated sessions reach the rail", () => {
  const children = [
    session("busy-sub", "subagent", { busy: true }),
    session("done-sub", "subagent", { busy: false, lastTurnStatus: "done" }),
    session("busy-wf", "workflow_agent", { busy: true }),
    session("failed-sub", "subagent", { busy: false, lastTurnStatus: "error" }),
    // A fork can be busy too, and it is not delegated work — it is a sibling
    // conversation, and it belongs in the tree, not pinned under the composer.
    session("busy-fork", "fork", { busy: true }),
  ];
  assert.deepEqual(liveSubagents(children).map((s) => s.id), ["busy-sub", "busy-wf"]);
});

Deno.test("a finished agent leaves the rail with no cleanup pass", () => {
  // `busy` is the server's derived "a turn is running", flipped by message.started /
  // message.finished in the store. Nothing here schedules a sweep or a timer.
  const running = session("a", "subagent", { busy: true });
  assert.equal(liveSubagents([running]).length, 1);
  assert.equal(liveSubagents([{ ...running, busy: false, lastTurnStatus: "done" }]).length, 0);
});

Deno.test("rail order is start order, so ⏎ opens what the cursor is on", () => {
  const later = session("later", "subagent", { busy: true, createdAt: 2000 });
  const earlier = session("earlier", "subagent", { busy: true, createdAt: 1000 });
  assert.deepEqual(liveSubagents([later, earlier]).map((s) => s.id), ["earlier", "later"]);
});

Deno.test("an empty rail is nothing at all, not an empty box", () => {
  assert.equal(SubagentRail({ branches: [], sel: null }), null);
});

Deno.test("the row and the hint say the one thing the rail means", () => {
  const s = session("x", "subagent", { title: "subagent · review app.ts", busy: true });
  assert.equal(railLabel(s), "review app.ts — ⋯ working");
  assert.equal(railHint(1), "↓ 1 subagent working");
  assert.equal(railHint(3), "↓ 3 subagents working");
});
