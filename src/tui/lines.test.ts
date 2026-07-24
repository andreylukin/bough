import { assertEquals, assertStringIncludes } from "jsr:@std/assert@1";
import { type Branch, buildLines, messageLines, parseSubagentNote } from "./lines.ts";
import { flattenTree } from "./components/SessionPicker.tsx";
import type { TuiSession } from "./store.ts";
import { buildTree, treeItems } from "./components/ConversationTree.tsx";
import type { Message } from "../schema/parts.ts";

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

Deno.test("parseSubagentNote: an orphan's 'unknown' file list is unknown, not a file", () => {
  const note = [
    '[subagent finished] "Stranded" (abc-123) — ORPHANED — the server restarted before it finished.',
    "Changed files on its branch: unknown.",
    "No report.",
    'Its changes stay on its own branch — adopt("abc-123") in run_steps merges them into this workspace; or leave the branch for review.',
  ].join("\n");
  const p = parseSubagentNote(note);
  assertEquals(p?.ok, false);
  assertEquals(p?.files, []);
  assertEquals(p?.filesUnknown, true);
});

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
  // The ▾ glyph sits outside the dim span (fold affordance), so match parts.
  assertEquals(
    expanded.some((l) => l.text.includes("▾") && l.text.includes("thinking (3 lines)")),
    true,
  );
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
  // Orphaned = the server restarted, not the agent's fault — never "failed"/red.
  assertEquals(card("orphaned").includes("interrupted — server restarted"), true);
  assertEquals(card("orphaned").includes("failed"), false);
  assertEquals(card("interrupted").includes("interrupted"), true);
  assertEquals(card("done").includes("done"), true);
  // A failed one must NOT read as done.
  assertEquals(card("error").includes("✗"), true);
});

Deno.test("branch card: a running subagent is left to the rail, not drawn in the transcript", () => {
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
  const branch: Branch = {
    id: "sub-x",
    title: "subagent · do the risky thing",
    busy: true,
    note: null,
    originMessageId: "a1",
  };
  const running = buildLines(thread, {}, () => false, () => false, 100, [branch]);
  assertEquals(running.some((l) => l.text.includes("do the risky thing")), false);
  // Once it finishes, the transcript keeps the card (the report lives there).
  const done = buildLines(thread, {}, () => false, () => false, 100, [
    { ...branch, busy: false, status: "done", ok: true, checkPassed: true },
  ]);
  assertEquals(done.some((l) => l.text.includes("do the risky thing")), true);
});

Deno.test("branch card: persisted {ok, checkPassed} gates the green ✓ — failed work never reads done", () => {
  // The harness catches program errors at the subagent boundary, so a subagent
  // whose work FAILED can still end its turn status=done. The persisted agent()
  // outcome breaks the tie: green "✓ done" only for ok && checkPassed.
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
  const card = (outcome: Pick<Branch, "status" | "ok" | "checkPassed">) => {
    const branch: Branch = {
      id: "sub-x",
      title: "subagent · do the risky thing",
      busy: false,
      note: null,
      ...outcome,
    };
    const lines = buildLines(thread, {}, () => false, () => false, 100, [branch]);
    return lines.find((l) => l.text.includes("do the risky thing"))!.text;
  };
  // ok + check passed: the only green "✓ done".
  assertEquals(card({ status: "done", ok: true, checkPassed: true }).includes("✓ done"), true);
  // ok but unchecked: done, flagged.
  assertEquals(
    card({ status: "done", ok: true, checkPassed: false }).includes("✓ done (check failed)"),
    true,
  );
  // !ok reads failed even when the turn status alone says done.
  assertEquals(card({ status: "done", ok: false, checkPassed: false }).includes("✗ failed"), true);
  // interrupted keeps its own marker (ok:false must not repaint it as failed).
  assertEquals(
    card({ status: "interrupted", ok: false, checkPassed: false }).includes("◼ interrupted"),
    true,
  );
  // Legacy rows (no persisted outcome) keep the status-only mapping.
  assertEquals(card({ status: "done" }).includes("✓ done"), true);
  assertEquals(card({ status: "done" }).includes("check failed"), false);
});

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

Deno.test("messageLines renders a prose part behind the accent gutter", () => {
  const msg: Message = {
    id: "m3",
    sessionId: "s1",
    role: "supervisor",
    parts: [{ type: "prose", text: "# Done\n- fixed the bug" }],
    pending: false,
    createdAt: 1,
  };
  const lines = messageLines(msg, () => false, () => false, 80);
  const joined = lines.map((l) => l.text).join("\n");
  assertStringIncludes(joined, "▎"); // every prose line carries the accent gutter
  assertStringIncludes(joined, "Done");
  // Right-click copy yields the raw markdown, not the styled render.
  assertEquals(lines.find((l) => l.copy)?.copy, "# Done\n- fixed the bug");
});

Deno.test("messageLines shows streamed log lines under a running tool call", () => {
  const msg: Message = {
    id: "m1",
    sessionId: "s1",
    role: "supervisor",
    parts: [
      { type: "tool_call", id: "c1", name: "run_steps", input: { code: "console.log()" } },
    ],
    pending: true,
    createdAt: 1,
  };
  const toolLogs = { c1: ["first", "second"] };
  const joined = messageLines(msg, () => true, () => false, 80, undefined, toolLogs)
    .map((l) => l.text).join("\n");
  assertStringIncludes(joined, "first");
  assertStringIncludes(joined, "second");
  // Once the result lands, the live buffer is replaced by the finalized output —
  // the same lines render once, from the tool_result, not duplicated.
  const done: Message = {
    ...msg,
    parts: [
      ...msg.parts,
      { type: "tool_result", callId: "c1", output: "first\nsecond", isError: false },
    ],
  };
  const doneJoined = messageLines(done, () => true, () => false, 80, undefined, toolLogs)
    .map((l) => l.text).join("\n");
  assertStringIncludes(doneJoined, "first");
});

Deno.test("interrupted tool result: partial output kept, ⏹ interrupted instead of ✓ done", () => {
  const msg: Message = {
    id: "m1",
    sessionId: "s1",
    role: "supervisor",
    parts: [
      { type: "tool_call", id: "c1", name: "run_steps", input: { code: "loop()" } },
      {
        type: "tool_result",
        callId: "c1",
        output: "tick-1\ntick-2\n[program error] program interrupted by the user",
        isError: false,
        interrupted: true,
      },
    ],
    pending: false,
    createdAt: 1,
  };
  const joined = messageLines(msg, () => true, () => false, 80)
    .map((l) => l.text).join("\n");
  // The ticks the program already printed survive the interrupt…
  assertStringIncludes(joined, "tick-1");
  assertStringIncludes(joined, "tick-2");
  // …under an interrupted marker, not a green check.
  assertStringIncludes(joined, "⏹ interrupted");
  assertEquals(joined.includes("✓ done"), false);
});

Deno.test("bg job cards: a running shell looks alive (marker + tail), a killed one honest, exits skip", () => {
  const thread = [
    {
      id: "u1",
      sessionId: "s",
      role: "user",
      parts: [{ type: "text", text: "start the server" }],
      pending: false,
    },
  ] as unknown as Message[];
  const jobs = [
    {
      id: "bg_1",
      command: "npm run dev",
      startedAt: Date.now() - 5_000,
      status: "running" as const,
      tailLines: ["listening on :3000"],
    },
    {
      id: "bg_2",
      command: "sleep 999",
      startedAt: Date.now(),
      status: "killed" as const,
      tailLines: [],
    },
    {
      id: "bg_3",
      command: "make build",
      startedAt: Date.now(),
      status: "exited" as const,
      tailLines: ["done"],
    },
  ];
  const joined = buildLines(thread, {}, () => false, () => false, 100, [], undefined, jobs)
    .map((l) => l.text).join("\n");
  // Running: alive marker + command + output tail.
  assertStringIncludes(joined, "bg_1");
  assertStringIncludes(joined, "⋯ running");
  assertStringIncludes(joined, "npm run dev");
  assertStringIncludes(joined, "listening on :3000");
  // Killed: honest outcome, not "done".
  assertStringIncludes(joined, "bg_2");
  assertStringIncludes(joined, "✗ killed");
  // A natural exit renders no card — its completion note is already in the thread.
  assertEquals(joined.includes("bg_3"), false);
});
Deno.test("a user message steered into a running turn carries a queued ack; it clears once a reply follows", () => {
  const msg = (id: string, role: string, text: string, pending = false) =>
    ({ id, sessionId: "s", role, parts: [{ type: "text", text }], pending }) as unknown as Message;
  const ACK = "queued — the agent will see this after the current step";
  // Mid-flight: the reply is pending and the user message landed after it.
  const midFlight = buildLines(
    [
      msg("u1", "user", "go"),
      msg("a1", "supervisor", "working", true),
      msg("u2", "user", "also do X"),
    ],
    {},
    () => false,
    () => false,
    100,
  ).map((l) => l.text).join("\n");
  assertStringIncludes(midFlight, ACK);
  // Once the follow-up turn starts, its reply follows the message — no ack.
  const drained = buildLines(
    [
      msg("u1", "user", "go"),
      msg("a1", "supervisor", "done"),
      msg("u2", "user", "also do X"),
      msg("a2", "supervisor", "on it", true),
    ],
    {},
    () => false,
    () => false,
    100,
  ).map((l) => l.text).join("\n");
  assertEquals(drained.includes(ACK), false);
  // A user message before the pending reply (the turn's own prompt) gets no ack.
  const normal = buildLines(
    [msg("u1", "user", "go"), msg("a1", "supervisor", "working", true)],
    {},
    () => false,
    () => false,
    100,
  ).map((l) => l.text).join("\n");
  assertEquals(normal.includes(ACK), false);
});
