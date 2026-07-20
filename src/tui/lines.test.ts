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

Deno.test("image part renders as a compact placeholder line", () => {
  const msg: Message = {
    id: "m1",
    sessionId: "s",
    role: "user",
    pending: false,
    createdAt: 1,
    parts: [
      { type: "text", text: "what does this graph mean? @shot.png" },
      {
        type: "image",
        path: "/home/u/.bough/attachments/x.png",
        mediaType: "image/png",
        name: "shot.png",
        size: 34_567,
      },
    ],
  };
  const lines = messageLines(msg, () => false, () => false, 120);
  const img = lines.find((l) => l.text.includes("\u{1F5BC}"))!;
  assertEquals(img.text.includes("shot.png (34 KB)"), true);
  // Right-click copy yields the attachment path.
  assertEquals(img.copy, "/home/u/.bough/attachments/x.png");
});

import { type Branch, buildLines } from "./lines.ts";

Deno.test("finished-subagent card caps the report; !full lifts the cap", () => {
  const sessionId = "sub-1";
  const report = Array.from({ length: 20 }, (_, i) => `report line ${i + 1}`).join("\n");
  const noteText = [
    `[subagent finished] "extract token logic" (${sessionId}) — DONE.`,
    "Changed files on its branch: token.ts.",
    "Report:",
    report,
    'Its changes stay on its own branch; adopt("x") to merge them.',
  ].join("\n");
  const thread = [
    {
      id: "u1",
      sessionId: "s",
      role: "user",
      parts: [{ type: "text", text: "go" }],
      pending: false,
    },
    {
      id: "a1",
      sessionId: "s",
      role: "supervisor",
      parts: [{ type: "text", text: "delegating" }],
      pending: false,
    },
    {
      id: "n1",
      sessionId: "s",
      role: "system",
      parts: [{ type: "text", text: noteText }],
      pending: false,
    },
  ] as unknown as Message[];
  const branch = {
    id: sessionId,
    title: "subagent · extract token logic",
    busy: false,
    originMessageId: "a1",
    note: parseSubagentNote(noteText),
  };

  // Collapsed: the report is capped, with a "show all" affordance.
  const capped = buildLines(thread, {}, () => false, () => false, 100, [branch]);
  const reportLines = capped.filter((l) => l.click === `report:${sessionId}`);
  const moreLine = capped.find((l) => l.text.includes("more · click to show all"));
  assertEquals(reportLines.length <= 6, true); // REPORT_LINES cap
  assertEquals(!!moreLine, true);
  assertEquals(moreLine!.click, `report:${sessionId}!full`);
  // The last report line is NOT shown while capped.
  assertEquals(capped.some((l) => l.text.includes("report line 20")), false);

  // Full (its !full toggle set): the whole report renders, no "+N more".
  const isFull = (k: string) => k === `report:${sessionId}`;
  const expanded = buildLines(thread, {}, () => false, isFull, 100, [branch]);
  assertEquals(expanded.some((l) => l.text.includes("report line 20")), true);
  assertEquals(expanded.some((l) => l.text.includes("more · click to show all")), false);
});

Deno.test("branch card: a finished blocking subagent reflects its real status (failed/interrupted, not always ✓ done)", () => {
  // A blocking agent()'s result flows in-band — no [subagent finished] note — so
  // the branch has note=null. The card now reads the session's lastTurnStatus so
  // a subagent that ERRORED or was INTERRUPTED no longer masquerades as "✓ done".
  const thread = [
    {
      id: "u1",
      sessionId: "s",
      role: "user",
      parts: [{ type: "text", text: "go" }],
      pending: false,
    },
    {
      id: "a1",
      sessionId: "s",
      role: "supervisor",
      parts: [{ type: "text", text: "delegating" }],
      pending: false,
    },
  ] as unknown as Message[];
  const card = (status: Branch["status"]) => {
    const branch: Branch = {
      id: "sub-x",
      title: "subagent · do the risky thing",
      busy: false,
      status,
      note: null,
    };
    const lines = buildLines(thread, {}, () => false, () => false, 100, [branch]);
    return lines.find((l) => l.text.includes("do the risky thing"))!.text;
  };
  assertEquals(card("error").includes("failed"), true);
  assertEquals(card("orphaned").includes("failed"), true);
  assertEquals(card("interrupted").includes("interrupted"), true);
  assertEquals(card("done").includes("done"), true);
  // A failed one must NOT read as done.
  assertEquals(card("error").includes("✗"), true);
});

import { assertStringIncludes } from "jsr:@std/assert@1";

Deno.test("messageLines renders a settled ask part as an always-visible Q → A line", () => {
  const msg: Message = {
    id: "m1",
    sessionId: "s1",
    role: "supervisor",
    parts: [{
      type: "ask",
      id: "q1",
      question: "Which env?",
      options: ["dev", "prod"],
      status: "answered",
      answer: "prod",
    }],
    pending: false,
    createdAt: 1,
  };
  const joined = messageLines(msg, () => false, () => false, 80).map((l) => l.text).join("\n");
  assertStringIncludes(joined, "Which env?");
  assertStringIncludes(joined, "prod");
  // Copy payload carries the full exchange.
  const line = messageLines(msg, () => false, () => false, 80).find((l) => l.copy);
  assertStringIncludes(line?.copy ?? "", "Which env? → prod");
});

Deno.test("messageLines renders a declined ask part", () => {
  const msg: Message = {
    id: "m2",
    sessionId: "s1",
    role: "supervisor",
    parts: [{ type: "ask", id: "q2", question: "Push to main?", status: "declined" }],
    pending: false,
    createdAt: 1,
  };
  const joined = messageLines(msg, () => false, () => false, 80).map((l) => l.text).join("\n");
  assertStringIncludes(joined, "Push to main?");
  assertStringIncludes(joined, "declined");
});
