/**
 * Tests for the live-work rail.
 *
 * One rule, stated three ways: **the rail pins LIVE work only.** A finished branch
 * belongs to the tree and to its report note, both of which outlive the run; a rail that
 * keeps everything it ever saw grows past the terminal on any real fan-out and pushes
 * the composer off screen — which is how the two agents actually working become the part
 * you cannot see. The old tree shipped that bug and fixed it in commit `0b56e12`; these
 * tests are what stop it coming back.
 *
 * `liveSubagents` is the lineage half of that rule, so it is tested directly rather than
 * through a render: it is a pure filter over the drill-in children of the open session.
 * What a row SAYS is `unitLine`'s (format.ts) and what the rail HOLDS is `liveUnits`'s
 * (store.ts); both are tested where they live.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import type { SessionKind } from "../../schema/parts.ts";
import type { SessionRow } from "../api.ts";
import type { LiveUnit } from "../store.ts";
import { liveSubagents, railHint, SubagentRail } from "./SubagentRail.tsx";

let clock = 1_700_000_000_000;

function session(
  id: string,
  kind: SessionKind,
  over: Partial<SessionRow> = {},
): SessionRow {
  return { id, title: id, kind, createdAt: clock++, parentId: null, busy: false, ...over };
}

test("only running delegated sessions reach the rail", () => {
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

test("a finished agent leaves the rail with no cleanup pass", () => {
  // `busy` is the server's derived "a turn is running", flipped by message.started /
  // message.finished in the store. Nothing here schedules a sweep or a timer.
  const running = session("a", "subagent", { busy: true });
  assert.equal(liveSubagents([running]).length, 1);
  assert.equal(liveSubagents([{ ...running, busy: false, lastTurnStatus: "done" }]).length, 0);
});

test("rail order is start order, so ⏎ opens what the cursor is on", () => {
  const later = session("later", "subagent", { busy: true, createdAt: 2000 });
  const earlier = session("earlier", "subagent", { busy: true, createdAt: 1000 });
  assert.deepEqual(liveSubagents([later, earlier]).map((s) => s.id), ["earlier", "later"]);
});

test("an empty rail is nothing at all, not an empty box", () => {
  assert.equal(SubagentRail({ units: [], sel: null, width: 80 }), null);
});

function unit(kind: LiveUnit["kind"], id: string): LiveUnit {
  return {
    kind,
    id,
    sessionId: id,
    title: id,
    elapsedMs: 1000,
    tokens: null,
    costUsd: null,
    progress: null,
    detail: null,
  };
}

test("the hint counts by kind — three shells and three agents are different news", () => {
  assert.equal(railHint([unit("shell", "bg_1")]), "↓ 1 shell running");
  assert.equal(
    railHint([unit("shell", "bg_1"), unit("shell", "bg_2"), unit("subagent", "a")]),
    "↓ 2 shells · 1 agent running",
  );
  assert.equal(railHint([unit("workflow", "run")]), "↓ 1 run running");
});

test("schedules are counted apart — 'running' would be a lie about a countdown", () => {
  assert.equal(railHint([unit("schedule", "s1"), unit("schedule", "s2")]), "↓ 2 scheduled");
  assert.equal(
    railHint([unit("shell", "bg_1"), unit("schedule", "s1")]),
    "↓ 1 shell running · 1 scheduled",
  );
});
