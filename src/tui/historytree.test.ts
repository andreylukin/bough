/**
 * The conversation tree — pi's `/tree`, asserted on fixtures with no terminal.
 */
import { test } from "bun:test";
import assert from "node:assert/strict";
import { historyTreeRows, messageGist, selectionFor } from "./historytree.ts";
import type { Message } from "../schema/parts.ts";
import type { SessionRow } from "./api.ts";

const msg = (id: string, role: Message["role"], text: string): Message => ({
  id,
  role,
  parts: text ? [{ type: "text", text }] : [],
  pending: false,
  createdAt: Number(id.replace(/\D/g, "")) || 1,
} as Message);

const branch = (
  id: string,
  at: string,
  title: string,
  busy = false,
): SessionRow => ({
  id,
  title,
  kind: "fork",
  originMessageId: at,
  createdAt: 1,
  busy,
} as unknown as SessionRow);

const THREAD = [
  msg("m1", "user", "add a discount function"),
  msg("m2", "supervisor", "done, it multiplies by (1 - pct/100)"),
  msg("m3", "user", "now validate pct"),
];

test("every turn is a node, and the last one is the active leaf", () => {
  const rows = historyTreeRows({ thread: THREAD, branches: [] });
  assert.equal(rows.length, 3);
  assert.deepEqual(rows.map((r) => r.kind), ["message", "message", "message"]);
  assert.ok(rows[0].text.includes("you add a discount function"));
  assert.ok(rows[1].text.includes("bough done, it multiplies"));
  // pi marks where the next turn will append. Exactly one row carries it.
  assert.equal(rows.filter((r) => r.active).length, 1);
  assert.ok(rows[2].active && rows[2].text.includes("← active"));
});

test("a branch hangs off the turn it cut from, oldest first", () => {
  const rows = historyTreeRows({
    thread: THREAD,
    branches: [branch("s2", "m1", "second attempt"), branch("s1", "m1", "first attempt")],
  });
  // Both branches sit directly under m1, before m2 — which is what makes the tree
  // answer "what else did I try here".
  assert.equal(rows[0].id, "m1");
  assert.deepEqual(rows.slice(1, 3).map((r) => r.kind), ["branch", "branch"]);
  assert.ok(rows[1].text.includes("⑂"));
  assert.equal(rows[3].id, "m2");
  // A running branch says so, so the tree doubles as a view of live work.
  const live = historyTreeRows({ thread: THREAD, branches: [branch("s3", "m2", "x", true)] });
  assert.ok(live.find((r) => r.kind === "branch")?.text.includes("⋯ working"));
});

test("user-only is a filter on the rows, not on the leaf", () => {
  const rows = historyTreeRows({ thread: THREAD, branches: [], userOnly: true });
  assert.deepEqual(rows.map((r) => r.id), ["m1", "m3"]);
  // The leaf is still the thread's real end, not the last row that survived.
  assert.ok(rows[1].active);
});

test("a turn with no prose is still a node — it is somewhere you can go back to", () => {
  const toolOnly = {
    id: "m9",
    role: "supervisor",
    parts: [{ type: "tool_call", id: "c1", name: "run_steps", input: {} }],
    pending: false,
    createdAt: 9,
  } as unknown as Message;
  assert.equal(messageGist(toolOnly), "(1 step)");
  assert.equal(historyTreeRows({ thread: [toolOnly], branches: [] }).length, 1);
});

test("a long gist is truncated with an ellipsis, a short one is left alone", () => {
  const long = "please refactor the discount function so it also handles tiered pricing rules";
  assert.ok(long.length > 56);
  const gist = messageGist(msg("m4", "user", long));
  assert.ok(gist.endsWith("…"));
  assert.ok(gist.length <= 56);
  assert.ok(long.startsWith(gist.slice(0, -1)));
  // Under the limit the gist IS the message, no marker added.
  assert.equal(messageGist(msg("m5", "user", "now validate pct")), "now validate pct");
});

test("Enter follows pi's selection rules", () => {
  const rows = historyTreeRows({ thread: THREAD, branches: [branch("s1", "m1", "other")] });
  // A USER turn cuts BEFORE itself and hands its text back, so you edit and
  // re-send — pi's "leaf set to parent, message text placed in editor".
  const onUser = selectionFor(rows[0], THREAD);
  assert.deepEqual(onUser, {
    fork: { atMessageId: "m1", exclusive: true },
    editorText: "add a discount function",
  });
  // Anything else cuts inclusive with an empty composer — pi's "leaf set to
  // selected node, editor stays empty".
  const assistantRow = rows.find((r) => r.id === "m2")!;
  assert.deepEqual(selectionFor(assistantRow, THREAD), { fork: { atMessageId: "m2" } });
  // A branch row is a session: open it.
  const branchRow = rows.find((r) => r.kind === "branch")!;
  assert.deepEqual(selectionFor(branchRow, THREAD), { open: "s1" });
});
