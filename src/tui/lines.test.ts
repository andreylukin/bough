import { assertEquals } from "jsr:@std/assert@1";
import { parseSubagentNote } from "./lines.ts";

Deno.test("parseSubagentNote: extracts fields from the completion note", () => {
  const note = [
    '[subagent finished] "Web Frontend Analysis" (940e12a0-c614-4c4b-9fa1-dc2ec419338d) — finished, check passed.',
    "Changed files on its branch: .serena/.gitignore, .serena/project.yml.",
    "Report:",
    "# Findings\nThe web layer uses React.",
    'Its changes stay on its own branch — adopt("940e12a0-c614-4c4b-9fa1-dc2ec419338d") in run_steps merges them into this workspace; or leave the branch for review.',
  ].join("\n");
  const p = parseSubagentNote(note);
  assertEquals(p?.title, "Web Frontend Analysis");
  assertEquals(p?.sessionId, "940e12a0-c614-4c4b-9fa1-dc2ec419338d");
  assertEquals(p?.ok, true);
  assertEquals(p?.files, [".serena/.gitignore", ".serena/project.yml"]);
  assertEquals(p?.report, "# Findings\nThe web layer uses React.");
});

Deno.test("parseSubagentNote: FAILED status + no files", () => {
  const note = [
    '[subagent finished] "Broken" (abc-123) — FAILED (turn errored or was interrupted).',
    "Changed files on its branch: none.",
    "No report.",
    'Its changes stay on its own branch — adopt("abc-123") in run_steps merges them into this workspace; or leave the branch for review.',
  ].join("\n");
  const p = parseSubagentNote(note);
  assertEquals(p?.ok, false);
  assertEquals(p?.files, []);
  assertEquals(p?.report, null);
});

Deno.test("parseSubagentNote: non-note text returns null", () => {
  assertEquals(parseSubagentNote("just a normal message"), null);
});

import { flattenTree } from "./components/SessionPicker.tsx";
import type { TuiSession } from "./store.ts";

function sess(p: Partial<TuiSession> & { id: string; kind: string }): TuiSession {
  return { title: p.id, createdAt: 1, ...p } as TuiSession;
}

Deno.test("flattenTree: nests by originId (what it branched from), not parentId", () => {
  // root ← fork ← subagent(of the fork); a fork of the fork nests under the fork.
  const rows = flattenTree([
    sess({ id: "root", kind: "root", createdAt: 1 }),
    sess({ id: "fork1", kind: "fork", originId: "root", parentId: null, createdAt: 2 }),
    sess({ id: "sub", kind: "subagent", originId: "fork1", createdAt: 3 }),
    // a fork of fork1 that carries a stale parentId=root — must still nest under fork1
    sess({ id: "fork2", kind: "fork", originId: "fork1", parentId: "root", createdAt: 4 }),
  ]);
  const depth = Object.fromEntries(rows.map((r) => [r.s.id, r.depth]));
  assertEquals(depth, { root: 0, fork1: 1, sub: 2, fork2: 2 });
});

Deno.test("flattenTree: connectors — last child gets └, earlier ├, with │ spine", () => {
  const rows = flattenTree([
    sess({ id: "r", kind: "root", createdAt: 1 }),
    sess({ id: "a", kind: "fork", originId: "r", createdAt: 2 }),
    sess({ id: "a1", kind: "subagent", originId: "a", createdAt: 3 }),
    sess({ id: "b", kind: "compaction", originId: "r", createdAt: 4 }),
  ]);
  const pref = Object.fromEntries(rows.map((r) => [r.s.id, r.prefix]));
  assertEquals(pref.r, ""); // trunk, no connector
  assertEquals(pref.a, "├─"); // first child of r (b comes after)
  assertEquals(pref.a1, "│ └─"); // only child of a, but r's spine continues (│)
  assertEquals(pref.b, "└─"); // last child of r
});

Deno.test("flattenTree: a root with an unknown origin surfaces as a trunk", () => {
  const rows = flattenTree([
    sess({ id: "orphan", kind: "fork", originId: "gone", createdAt: 1 }),
  ]);
  assertEquals(rows.length, 1);
  assertEquals(rows[0].depth, 0);
});

import { buildTree, treeItems } from "./components/ConversationTree.tsx";
import type { Message } from "../schema/parts.ts";

function msg(id: string, role: string, text = ""): Message {
  return {
    id,
    sessionId: "s",
    role,
    parts: text ? [{ type: "text", text }] : [],
    pending: false,
  } as Message;
}

Deno.test("buildTree: user turns are nodes, branches attach to their origin turn", () => {
  const thread = [
    msg("u1", "user", "analyze auth"),
    msg("a1", "supervisor", "ok done"),
    msg("u2", "user", "now fix it"),
    msg("a2", "supervisor", "fixed"),
  ];
  const branches = [
    sess({ id: "sub", kind: "subagent", originId: "s", originMessageId: "a1", createdAt: 2 }),
    sess({ id: "fork", kind: "fork", originId: "s", originMessageId: "u2", createdAt: 3 }),
  ];
  const nodes = buildTree(thread, branches);
  assertEquals(nodes.map((n) => n.msg.id), ["u1", "u2"]);
  assertEquals(nodes[0].branches.map((b) => b.id), ["sub"]); // sub spawned during turn 1
  assertEquals(nodes[1].branches.map((b) => b.id), ["fork"]); // fork split at u2
  assertEquals(nodes[1].tip, true); // last turn is the live tip
});

Deno.test("buildTree: tool runs become branch points cutting at the result part", () => {
  const asst = {
    id: "a1",
    sessionId: "s",
    role: "supervisor",
    parts: [
      { type: "tool_call", id: "c1", name: "run_steps", input: { code: "read()" } },
      { type: "tool_result", callId: "c1", output: "ok" },
      { type: "tool_call", id: "c2", name: "run_steps", input: { code: "edit()" } },
      { type: "tool_result", callId: "c2", output: "done" },
    ],
    pending: false,
  } as unknown as Message;
  const [node] = buildTree([msg("u1", "user", "go"), asst], []);
  assertEquals(node.steps.map((s) => s.point.atPart), [1, 3]); // cut through each result
  assertEquals(node.steps[0].label.startsWith("run_steps"), true);
});

Deno.test("treeItems: node, then its tool steps, then its branches", () => {
  const asst = {
    id: "a1",
    sessionId: "s",
    role: "supervisor",
    parts: [{ type: "tool_call", id: "c1", name: "run_steps", input: {} }],
    pending: false,
  } as unknown as Message;
  const items = treeItems(buildTree([msg("u1", "user", "hi"), asst], [
    sess({ id: "b", kind: "subagent", originId: "s", originMessageId: "a1", createdAt: 1 }),
  ]));
  assertEquals(
    items.map((it) => it.type === "node" ? "node" : it.type === "step" ? "step" : "branch"),
    ["node", "step", "branch"],
  );
});

import { messageLines } from "./lines.ts";

Deno.test("collapsed tool fold carries a gist of what ran", () => {
  const msg: Message = {
    id: "m1",
    sessionId: "s",
    role: "supervisor",
    pending: false,
    createdAt: 1,
    parts: [
      {
        type: "tool_call",
        id: "c1",
        name: "run_steps",
        input: { code: "// setup\nsh('curl -sS https://example.com')" },
      },
      { type: "tool_result", callId: "c1", output: "200", isError: false },
    ],
  };
  const collapsed = messageLines(msg, () => false, () => false, 120);
  const head = collapsed.find((l) => l.text.includes("tool call"))!;
  // Gist = first meaningful code line (comments skipped), on the fold header.
  assertEquals(head.text.includes("sh('curl -sS https://example.com')"), true);
  // Expanded shows the real thing — no gist on the header.
  const expanded = messageLines(msg, () => true, () => false, 120);
  const openHead = expanded.find((l) => l.text.includes("tool call"))!;
  assertEquals(openHead.text.includes("curl"), false);
});

Deno.test("reasoning folds: collapsed gist line, expanded gutter block", () => {
  const msg: Message = {
    id: "m1",
    sessionId: "s",
    role: "supervisor",
    pending: false,
    createdAt: 1,
    parts: [
      { type: "reasoning", text: "Let me look at the auth flow.\nSecond thought.\nThird." },
      { type: "text", text: "done" },
    ],
  } as unknown as Message;
  const collapsed = messageLines(msg, () => false, () => false, 120);
  const head = collapsed.find((l) => l.text.includes("thinking"))!;
  assertEquals(head.text.includes("▸"), true);
  assertEquals(head.text.includes("Let me look at the auth flow."), true);
  assertEquals(head.click, "m1:0"); // click expands
  // Body lines are hidden while collapsed.
  assertEquals(collapsed.some((l) => l.text.includes("Second thought.")), false);
  const expanded = messageLines(msg, () => true, () => false, 120);
  assertEquals(expanded.some((l) => l.text.includes("▾ thinking (3 lines)")), true);
  assertEquals(expanded.some((l) => l.text.includes("Second thought.")), true);
});
