import assert from "node:assert/strict";
import { test } from "bun:test";
import {
  type Branch,
  buildLines,
  chatBodyHeight,
  branchesFrom,
  jobCardLines,
  skillsNamed,
  lineAtSlot,
  type JobView,
  messageLines,
  parseBgNote,
  parseImageNote,
  parseSubagentNote,
  visibleSlice,
  type VLine,
} from "./lines.ts";
import { setColorEnabled, width } from "./format.ts";
import type { Message, Part } from "../schema/parts.ts";

// Nothing here needs a terminal. Color is forced OFF for the structural
// assertions so a folding rule is compared against words, not escape sequences —
// the fold decisions are independent of the palette by construction (format.ts,
// third invariant), and a test that matched on SGR codes would be asserting the
// theme instead of the rule.
setColorEnabled(false);

const OPEN = () => true;
const CLOSED = () => false;

function msg(p: Partial<Message> & { id: string }): Message {
  return {
    sessionId: "s1",
    role: "supervisor",
    parts: [],
    pending: false,
    createdAt: 1,
    ...p,
  } as Message;
}

const call = (id: string, code: string, name = "run_steps"): Part => ({
  type: "tool_call",
  id,
  name,
  input: { code },
});

const result = (callId: string, output: string, extra: Record<string, unknown> = {}): Part => ({
  type: "tool_result",
  callId,
  output,
  isError: false,
  ...extra,
} as Part);

const joined = (lines: VLine[]) => lines.map((l) => l.text).join("\n");

// ---- wrapping ---------------------------------------------------------------

test("every emitted line fits the width — long prose, long code, long output", () => {
  const w = 48;
  const m = msg({
    id: "m1",
    parts: [
      { type: "text", text: "alpha beta gamma delta epsilon zeta eta theta iota kappa ".repeat(4) },
      call("c1", "await bash('" + "x".repeat(200) + "')"),
      result("c1", "y".repeat(300)),
    ],
  });
  for (const l of messageLines(m, OPEN, CLOSED, w)) {
    assert.ok(width(l.text) <= w, `row ${width(l.text)} wide: ${JSON.stringify(l.text)}`);
  }
});

test("a hard-wrapped block keeps its gutter on every physical line", () => {
  const m = msg({
    id: "m1",
    parts: [call("c1", "const x = 1"), result("c1", "z".repeat(120))],
  });
  const out = messageLines(m, OPEN, CLOSED, 40).filter((l) => l.text.includes("z"));
  assert.ok(out.length >= 3); // 120 columns of output across a ~36-column gutter
  for (const l of out) assert.ok(l.text.trimStart().startsWith("│"), l.text);
});

// ---- folding: the program that ran ------------------------------------------

test("a collapsed tool step says what the program did", () => {
  const m = msg({
    id: "m1",
    parts: [
      call("c1", "// setup\nawait bash('deno test')"),
      result("c1", "ok"),
    ],
  });
  const collapsed = messageLines(m, CLOSED, CLOSED, 120);
  const head = collapsed.find((l) => l.text.includes("step"))!;
  assert.ok(head.text.includes("▸"), head.text); // closed
  // The tool NAME is gone from the header: bough has one tool, so repeating its
  // internal identifier per call was the whole summary of a multi-step turn.
  assert.equal(head.text.includes("run_steps"), false, head.text);
  // Was the clipped source line; now the operation, the way every comparable
  // harness labels a step (`Ran 1 shell command`). The code is one keypress away.
  assert.ok(head.text.includes("ran 1 command"), head.text);
  assert.equal(head.click, "m1:0"); // clicking the header toggles the fold
  // The body is hidden while collapsed — that is the entire point of the fold.
  assert.equal(joined(collapsed).includes("// setup"), false);
  assert.equal(joined(collapsed).includes("↳ output"), false);

  const expanded = messageLines(m, OPEN, CLOSED, 120);
  const openHead = expanded.find((l) => l.text.includes("step"))!;
  assert.ok(openHead.text.includes("▾"));
  // Expanded shows the real program, so the header drops the gist.
  assert.equal(openHead.text.includes("deno test"), false);
  assert.ok(joined(expanded).includes("// setup"));
  assert.ok(joined(expanded).includes("↳ output"));
});

test("one round of program + result is ONE step, not two entries", () => {
  const m = msg({
    id: "m1",
    parts: [call("c1", "a()"), result("c1", "1"), call("c2", "b()"), result("c2", "2")],
  });
  const heads = messageLines(m, CLOSED, CLOSED, 120).filter((l) => l.text.includes("steps"));
  assert.equal(heads.length, 1);
  assert.ok(heads[0].text.includes("2 steps"));
});

test("a running call is visible on the collapsed header and shows live console output", () => {
  const m = msg({
    id: "m1",
    pending: true,
    parts: [call("c1", "console.log('x')")],
  });
  const logs = { c1: ["first", "second"] };
  const head = messageLines(m, CLOSED, CLOSED, 120, undefined, logs)
    .find((l) => l.text.includes("step"))!;
  // The live marker leads the row, where a 100-column screen cannot clip it, and
  // the summary is present tense while the call is still open.
  assert.ok(head.text.includes("⚙"), head.text);
  assert.ok(head.text.indexOf("⚙") < head.text.indexOf("step"), head.text);

  const live = joined(messageLines(m, OPEN, CLOSED, 120, undefined, logs));
  assert.ok(live.includes("↳ output (live)"));
  assert.ok(live.includes("first") && live.includes("second"));

  // Once the result lands the live buffer is REPLACED, not appended: the same
  // lines stream to the UI and batch into the tool result (spec §5), and the
  // transcript must show them once.
  const done = msg({ ...m, parts: [...m.parts, result("c1", "first\nsecond")] });
  const settled = joined(messageLines(done, OPEN, CLOSED, 120, undefined, logs));
  assert.equal(settled.includes("(live)"), false);
  assert.equal(settled.split("first").length - 1, 1);
});

test("caps: a long program and a long output truncate; only !full lifts them", () => {
  const code = Array.from({ length: 40 }, (_v, i) => `line ${i}`).join("\n");
  const out = Array.from({ length: 60 }, (_v, i) => `out ${i}`).join("\n");
  const m = msg({ id: "m1", parts: [call("c1", code), result("c1", out)] });

  const capped = joined(messageLines(m, OPEN, CLOSED, 120));
  assert.ok(capped.includes("line 13")); // CODE_LINES = 14
  assert.equal(capped.includes("line 14"), false);
  assert.ok(capped.includes("out 19")); // OUTPUT_LINES = 20
  assert.equal(capped.includes("out 20"), false);
  assert.ok(capped.includes("more lines"));

  // The cap-lift is its own key, so expand-all cannot dump the whole thing.
  const more = messageLines(m, OPEN, CLOSED, 120).find((l) => l.text.includes("more lines"))!;
  assert.equal(more.click, "m1:0!full");
  const full = joined(messageLines(m, OPEN, () => true, 120));
  assert.ok(full.includes("line 39") && full.includes("out 59"));
  assert.equal(full.includes("more lines"), false);
});

test("an interrupted result keeps its partial output and never reads ✓ done", () => {
  const m = msg({
    id: "m1",
    parts: [
      call("c1", "loop()"),
      result("c1", "tick-1\ntick-2", { interrupted: true }),
    ],
  });
  const text = joined(messageLines(m, OPEN, CLOSED, 120));
  assert.ok(text.includes("tick-1") && text.includes("tick-2"));
  assert.ok(text.includes("⏹ interrupted"));
  assert.equal(text.includes("✓ done"), false);
  // …and it is legible without expanding.
  const head = messageLines(m, CLOSED, CLOSED, 120).find((l) => l.text.includes("step"))!;
  assert.ok(head.text.includes("⏹ interrupted"));
});

test("an errored result is marked on the closed header", () => {
  const m = msg({
    id: "m1",
    parts: [call("c1", "boom()"), result("c1", "[program error] boom", { isError: true })],
  });
  const head = messageLines(m, CLOSED, CLOSED, 120).find((l) => l.text.includes("step"))!;
  assert.ok(head.text.includes("✗ error"), head.text);
});

test("a `<stop>` the model wrote as prose is not shown to the user", () => {
  // `stop` is one of the two granted tools and a small model sometimes writes its name
  // instead of calling it. Live, the answer to "what colour is this image" ended with a
  // code block containing `<stop>` — harness plumbing as the last thing the reader sees.
  const fenced = msg({
    id: "m1",
    parts: [{ type: "text", text: "The image is filled with green.\n\n```\n<stop>\n```" }],
  });
  const out = joined(messageLines(fenced, CLOSED, CLOSED, 120));
  assert.ok(out.includes("filled with green"), out);
  assert.equal(out.includes("<stop>"), false, out);

  // A bare one on its own last line, too.
  const bare = msg({ id: "m2", parts: [{ type: "text", text: "Done.\n<stop>" }] });
  const out2 = joined(messageLines(bare, CLOSED, CLOSED, 120));
  assert.ok(out2.includes("Done."), out2);
  assert.equal(out2.includes("<stop>"), false, out2);

  // NOT stripped mid-message: a reply that is ABOUT the protocol keeps every word.
  const about = msg({
    id: "m3",
    parts: [{ type: "text", text: "Call `<stop>` in the same response as your final text." }],
  });
  assert.ok(joined(messageLines(about, CLOSED, CLOSED, 120)).includes("<stop>"));
});

test("declining a question is not a failed round", () => {
  // `ask()` throws when the user presses esc, so the round came back with an error result
  // and the header led with `✗ error` — red, for the user taking the option the card
  // itself offers ("esc decline"), with the receipt saying `→ declined` two rows below.
  const m = msg({
    id: "m1",
    parts: [
      call("c1", "await ask('Enable strict mode?', ['yes', 'no'])"),
      result("c1", "the user declined", { isError: true }),
      { type: "ask", id: "a1", question: "Enable strict mode?", status: "declined" },
    ],
  });
  const head = messageLines(m, CLOSED, CLOSED, 120).find((l) => l.text.includes("step"))!;
  assert.ok(head.text.includes("⏹ declined"), head.text);
  assert.equal(head.text.includes("✗ error"), false, head.text);

  // An ANSWERED question whose program then genuinely failed is still an error.
  const failed = msg({
    id: "m2",
    parts: [
      call("c1", "await ask('pick', ['a'])"),
      result("c1", "[program error] boom", { isError: true }),
      { type: "ask", id: "a1", question: "pick", status: "answered", answer: "a" },
    ],
  });
  const head2 = messageLines(failed, CLOSED, CLOSED, 120).find((l) => l.text.includes("step"))!;
  assert.ok(head2.text.includes("✗ error"), head2.text);
});

test("a round that failed once and then worked is amber with a count, not red", () => {
  // Live on a skill run that finished ✓ and did the work: the model reached for `patch` as
  // a tool, was told off, wrote the program properly — and the header still led with
  // `✗ error  2 steps`, which reads as "this round failed".
  const m = msg({
    id: "m1",
    parts: [
      call("c1", "", "patch"),
      result("c1", "unknown tool: patch", { isError: true }),
      call("c2", "await write('a.py', 'x')"),
      result("c2", "wrote a.py"),
    ],
  });
  const lines = messageLines(m, CLOSED, CLOSED, 120);
  const head = lines.find((l) => l.text.includes("step"))!;
  assert.ok(head.text.includes("1 of 2 failed"), head.text);
  assert.equal(head.text.includes("✗ error"), false, head.text);
  // And the INVENTED name is not printed as an identifier: the gist already explains it,
  // so the header read `patch · run_steps · called patch as a tool · …` — two internal
  // names, one of them fictional, in front of the prose that covers both.
  assert.equal(head.text.includes("patch · run_steps"), false, head.text);
  // Two steps, so each gets its OWN row under the header.
  const rows = lines.map((l) => l.text);
  assert.ok(rows.some((t) => t.includes("called patch as a tool")), rows.join("\n"));
  assert.ok(rows.some((t) => t.includes("wrote a.py")), rows.join("\n"));

  // Every call failing is still red: nothing recovered.
  const allBad = msg({
    id: "m2",
    parts: [
      call("c1", "boom()"),
      result("c1", "[program error] boom", { isError: true }),
      call("c2", "boom()"),
      result("c2", "[program error] boom", { isError: true }),
    ],
  });
  const head2 = messageLines(allBad, CLOSED, CLOSED, 120).find((l) => l.text.includes("step"))!;
  assert.ok(head2.text.includes("✗ error"), head2.text);
});

// ---- folding: reasoning -----------------------------------------------------

test("reasoning folds: a collapsed gist line, an expanded gutter block", () => {
  const m = msg({
    id: "m1",
    parts: [
      { type: "reasoning", text: "Let me look at the auth flow.\nSecond thought.\nThird." },
      { type: "text", text: "done" },
    ],
  });
  const collapsed = messageLines(m, CLOSED, CLOSED, 120);
  const head = collapsed.find((l) => l.text.includes("thinking"))!;
  assert.ok(head.text.includes("▸"));
  assert.ok(head.text.includes("Let me look at the auth flow."));
  assert.equal(head.click, "m1:0");
  assert.equal(joined(collapsed).includes("Second thought."), false);

  const expanded = messageLines(m, OPEN, CLOSED, 120);
  assert.ok(expanded.some((l) => l.text.includes("▾") && l.text.includes("thinking (3 lines)")));
  assert.ok(joined(expanded).includes("Second thought."));
  // The prose is never folded — it is the answer.
  assert.ok(joined(collapsed).includes("done"));
});

test("reasoning with no text renders nothing at all", () => {
  const m = msg({
    id: "m1",
    parts: [{ type: "reasoning", text: "  \n " }, { type: "text", text: "hi" }],
  });
  assert.equal(joined(messageLines(m, CLOSED, CLOSED, 120)).includes("thinking"), false);
});

// ---- other part kinds -------------------------------------------------------

test("a settled ask renders as one always-visible Q → A line", () => {
  const answered = msg({
    id: "m1",
    parts: [
      { type: "ask", id: "q1", question: "Ship it?", status: "answered", answer: "yes" },
    ],
  });
  const line = messageLines(answered, CLOSED, CLOSED, 120).find((l) =>
    l.text.includes("Ship it?")
  )!;
  assert.ok(line.text.includes("→") && line.text.includes("yes"));
  assert.equal(line.copy, "Ship it? → yes");

  const declined = msg({
    id: "m2",
    parts: [{ type: "ask", id: "q2", question: "Ship it?", status: "declined" }],
  });
  assert.ok(joined(messageLines(declined, CLOSED, CLOSED, 120)).includes("declined"));
});

test("an image part renders as a compact placeholder and copies its path", () => {
  const m = msg({
    id: "m1",
    role: "user",
    parts: [
      { type: "text", text: "what is this?" },
      {
        type: "image",
        path: "/home/u/.bough/attachments/x.png",
        mediaType: "image/png",
        name: "shot.png",
        size: 34_567,
      },
    ],
  });
  const img = messageLines(m, CLOSED, CLOSED, 120).find((l) => l.text.includes("🖼"))!;
  assert.ok(img.text.includes("shot.png (34 KB)"));
  assert.equal(img.copy, "/home/u/.bough/attachments/x.png");
});

test("an [image] system note collapses onto the placeholder — no role label, no path", () => {
  const m = msg({
    id: "m1",
    role: "system",
    parts: [
      { type: "text", text: "[image] /tmp/shot.png — the failing screen" },
      {
        type: "image",
        path: "/tmp/shot.png",
        mediaType: "image/png",
        name: "shot.png",
        size: 2048,
      },
    ],
  });
  const lines = messageLines(m, CLOSED, CLOSED, 120);
  assert.equal(lines.length, 2); // one spacer, one line
  assert.ok(lines[1].text.includes("shot.png — the failing screen · 2 KB"));
  assert.equal(joined(lines).includes("system"), false);
  assert.equal(joined(lines).includes("/tmp/shot.png"), false);
});

test("a workflow completion report is folded in the transcript", () => {
  const report = [
    '[workflow done] "audit all handlers" (wf-1) — 2/2 agents succeeded.',
    "Replay: not a relaunch — this run started fresh and journalled as it went.",
    "Result:",
    JSON.stringify({ findings: ["one", "two"] }, null, 2),
  ].join("\n");
  const m = msg({ id: "wf-note", role: "system", parts: [{ type: "text", text: report }] });
  const collapsed = messageLines(m, CLOSED, CLOSED, 120);
  const head = collapsed.find((l) => l.text.includes("audit all handlers"))!;
  assert.ok(head.text.includes("▸") && head.text.includes("2/2 agents succeeded"), head.text);
  assert.equal(joined(collapsed).includes('"findings"'), false);
  assert.equal(head.click, "wf-note:workflow");

  const expanded = messageLines(m, OPEN, CLOSED, 120);
  assert.ok(joined(expanded).includes('"findings"'));
  assert.ok(expanded.some((l) => l.text.includes("▾ workflow")));
});

// ---- system-note parsing ----------------------------------------------------

const NOTE = (status: string, files: string, report: string | null) =>
  [
    `[subagent finished] "extract token logic" (sub-1) — ${status}.`,
    `Changed files: ${files}.`,
    report ? `Report:\n${report}` : "No report.",
    "It worked in THIS session's checkout, so its edits are already here — read them " +
    "before building on top; there is nothing to merge.",
  ].join("\n");

test("parseSubagentNote: the real note shape from agents/notes.ts", () => {
  const p = parseSubagentNote(NOTE("finished", "a.ts, b.ts", "# Findings\nAll good."))!;
  assert.equal(p.title, "extract token logic");
  assert.equal(p.sessionId, "sub-1");
  assert.equal(p.ok, true);
  assert.deepEqual(p.files, ["a.ts", "b.ts"]);
  assert.equal(p.report, "# Findings\nAll good.");
});

test("parseSubagentNote: the four outcomes stay distinguishable", () => {
  assert.equal(parseSubagentNote(NOTE("finished", "none", null))!.ok, true);
  const failed = parseSubagentNote(
    NOTE("FAILED — its turn errored. Nothing retried it", "x", null),
  )!;
  assert.equal(failed.ok, false);
  assert.ok(failed.status.startsWith("FAILED"));
  assert.ok(
    parseSubagentNote(NOTE("STOPPED — it was interrupted", "x", null))!.status.startsWith(
      "STOPPED",
    ),
  );
  assert.ok(
    parseSubagentNote(NOTE("ORPHANED — the server restarted", "x", null))!.status.startsWith(
      "ORPHANED",
    ),
  );
  assert.equal(parseSubagentNote("just a normal message"), null);
});

test("parseSubagentNote: 'not reported' is unknown, not a file named so", () => {
  const p = parseSubagentNote(NOTE("finished", "not reported", null))!;
  assert.deepEqual(p.files, []);
  assert.equal(p.filesUnknown, true);
});

test("parseBgNote / parseImageNote", () => {
  assert.equal(
    parseBgNote('[background] bg_2 finished (exit 1) — command "make", 3 lines'),
    "bg_2",
  );
  assert.equal(parseBgNote("hello"), null);
  assert.deepEqual(parseImageNote("[image] /tmp/a.png — note"), {
    path: "/tmp/a.png",
    note: "note",
  });
  assert.deepEqual(parseImageNote("[image] /tmp/a.png"), { path: "/tmp/a.png", note: undefined });
  assert.equal(parseImageNote("not an image note"), null);
});

// ---- the whole transcript ---------------------------------------------------

const branch = (p: Partial<Branch> & { id: string }): Branch => ({
  title: p.id,
  busy: false,
  ...p,
});

test("a finished subagent's card replaces its raw note at the spawn point", () => {
  const noteText = NOTE("finished", "a.ts", "Found three call sites.");
  const thread = [
    msg({ id: "u1", role: "user", parts: [{ type: "text", text: "go" }] }),
    msg({ id: "a1", parts: [call("c1", "await agent('x')"), result("c1", "{}")] }),
    msg({ id: "n1", role: "system", parts: [{ type: "text", text: noteText }] }),
  ];
  const b = branch({
    id: "sub-1",
    title: "extract token logic",
    originMessageId: "a1",
    note: parseSubagentNote(noteText),
  });
  const text = joined(buildLines(thread, CLOSED, CLOSED, 100, { branches: [b] }));
  assert.equal(text.includes("[subagent finished]"), false); // the raw wall is gone
  assert.ok(text.includes("extract token logic"));
  assert.ok(text.includes("Found three call sites."));
  // The NEXT ACTION, and it has been wrong twice: "click to open" when no click was
  // dispatched, then `^s opens it` when `^s` opens the tree and is guarded on an empty
  // draft. Clicking is wired now (every row carries `open:<sessionId>`), so the row
  // names the pointer, and the tree by the chord that is not guarded.
  assert.ok(text.includes("its edits are already here"), text);
  assert.ok(text.includes("click to open it"), text);
  assert.equal(text.includes("^s opens it"), false, text);

  // With no branch to draw the card, the note itself must survive — otherwise the
  // report would render nowhere at all.
  const bare = joined(buildLines(thread, CLOSED, CLOSED, 100));
  assert.ok(bare.includes("[subagent finished]"));
});

test("a running subagent is left to the rail; a card with no spawn point tails out", () => {
  const thread = [msg({ id: "a1", parts: [{ type: "text", text: "working" }] })];
  const running = branch({ id: "sub-1", title: "live one", busy: true, originMessageId: "a1" });
  assert.equal(
    joined(buildLines(thread, CLOSED, CLOSED, 100, { branches: [running] })).includes("live one"),
    false,
  );
  // A branch whose spawn turn a fork dropped still renders, at the tail.
  const stranded = branch({ id: "sub-2", title: "stranded", originMessageId: "gone" });
  const out = joined(buildLines(thread, CLOSED, CLOSED, 100, { branches: [stranded] }));
  assert.ok(out.includes("subagents with no spawn point in this thread"));
  assert.ok(out.includes("stranded"));
});

test("branch cards state the real outcome — failed and orphaned never read done", () => {
  const thread = [msg({ id: "a1", parts: [{ type: "text", text: "x" }] })];
  const render = (b: Partial<Branch>) =>
    joined(buildLines(thread, CLOSED, CLOSED, 100, {
      branches: [branch({ id: "s", title: "child", originMessageId: "a1", ...b })],
    }));
  assert.ok(render({ status: "done", ok: true }).includes("✓ done"));
  assert.ok(render({ status: "error" }).includes("✗ failed"));
  assert.ok(render({ status: "done", ok: false }).includes("✗ failed"));
  assert.ok(render({ status: "interrupted" }).includes("◼ interrupted"));
  assert.ok(render({ status: "orphaned" }).includes("the server restarted"));

  // WHAT IT COST, next to the outcome. Measured against Claude Code's agent rail, which
  // keeps `12s · ↓ 18.0k tokens` per agent; bough's card said `✓ done` and nothing else
  // while the row it is built from carried the numbers.
  const paid = render({ status: "done", ok: true, tokens: 18000, costUsd: 0.031 });
  assert.ok(paid.includes("18k tok"), paid);
  assert.ok(paid.includes("$0.03"), paid);
  // Zero tokens is a fact, not missing data: an agent interrupted before its first call
  // really did bill nothing.
  assert.ok(render({ status: "interrupted", tokens: 0 }).includes("0 tok"));
});

test("a message steered into a running turn carries a queued ack until a reply follows", () => {
  const withPending = [
    msg({ id: "u1", role: "user", parts: [{ type: "text", text: "go" }] }),
    msg({ id: "a1", pending: true, parts: [{ type: "text", text: "working" }] }),
    msg({ id: "u2", role: "user", parts: [{ type: "text", text: "also this" }] }),
  ];
  assert.ok(joined(buildLines(withPending, CLOSED, CLOSED, 100)).includes("⧖ queued"));
  // The first user message, before any pending reply, is not queued.
  assert.equal(
    joined(buildLines(withPending.slice(0, 1), CLOSED, CLOSED, 100)).includes("⧖ queued"),
    false,
  );
});

test("marks land where they happened, and a destructive one outlives its toast", () => {
  // G4 + G11. What a turn cost used to vanish with the spinner, and a revert printed a
  // notice that expired ten seconds later — after which nothing anywhere said a file
  // had been thrown away. Both are marks now, and a mark is interleaved by TIME rather
  // than appended, so it stays under the turn it belongs to when the next one lands.
  const thread = [
    msg({ id: "u1", role: "user", parts: [{ type: "text", text: "go" }], createdAt: 10 }),
    msg({ id: "a1", parts: [{ type: "text", text: "done" }], createdAt: 20 }),
    msg({ id: "u2", role: "user", parts: [{ type: "text", text: "again" }], createdAt: 40 }),
  ];
  const marks = [
    { id: "m2", sessionId: "s1", at: 30, kind: "turn" as const, text: "✓ 14s · 3.2k tok · $0.021" },
    { id: "m1", sessionId: "s1", at: 15, kind: "destructive" as const, text: "reverted README.md" },
    { id: "m3", sessionId: "s1", at: 90, kind: "destructive" as const, text: "killed bg_7" },
  ];
  const rows = buildLines(thread, CLOSED, CLOSED, 100, { marks }).map((l) => l.text.trim());
  const at = (text: string) => rows.findIndex((r) => r === text);
  // Oldest first regardless of the order they were handed over, each in its own place.
  assert.ok(at("reverted README.md") > at("go"));
  assert.ok(at("reverted README.md") < at("done"));
  assert.ok(at("✓ 14s · 3.2k tok · $0.021") > at("done"));
  assert.ok(at("✓ 14s · 3.2k tok · $0.021") < at("again"));
  // A mark newer than every message still renders — at the tail, where it happened.
  assert.equal(at("killed bg_7"), rows.length - 1);
  // A copy yields the line itself, not the two columns of indent it hangs from.
  const killed = buildLines(thread, CLOSED, CLOSED, 100, { marks })
    .find((l) => l.text.trim() === "killed bg_7");
  assert.equal(killed?.copy, "killed bg_7");
});

test("job cards: a running shell looks alive, an exited one states its outcome", () => {
  const now = 100_000;
  const running: JobView = {
    id: "bg_1",
    name: "test run",
    sessionId: "s1",
    pid: 10,
    command: "deno test",
    status: "running",
    startedAt: now - 65_000,
    tail: ["running tests"],
    outputLines: 12,
  };
  const failed: JobView = {
    ...running,
    id: "bg_2",
    status: "exited",
    exitCode: 1,
    exitedAt: now,
    tail: ["FAILED"],
    outputLines: 1,
  };
  const out: VLine[] = [];
  jobCardLines(out, running, 80, now);
  jobCardLines(out, failed, 80, now);
  const text = joined(out);
  assert.ok(text.includes("⋯ running") && text.includes("1m 5s"));
  assert.ok(text.includes("running tests"));
  assert.ok(text.includes("12 lines total"));
  assert.ok(text.includes("✗ exit 1")); // the outcome survives the exit
  // Every card row is a door INTO that job — `job:<session>:<id>`, which `App`
  // resolves. The old target was the bare word "jobs" and nothing handled it.
  assert.ok(out.every((l) => l.click?.startsWith("job:s1:bg_") || l.text.trim() === ""));

  // A JOB THE USER KILLED IS NOT A JOB THAT SUCCEEDED. `exitCode` is null for a signalled
  // process and `?? 0` read that as success, so `x x` on a running shell produced
  // `⚙ sleep 120 ✓ done` — a green tick on the thing cancelled two seconds earlier.
  const killed: JobView = {
    ...running,
    id: "bg_3",
    status: "exited",
    exitCode: null,
    signal: "SIGTERM",
    exitedAt: now,
    tail: [],
    outputLines: 0,
  };
  const killedText = joined((() => {
    const o: VLine[] = [];
    jobCardLines(o, killed, 80, now);
    return o;
  })());
  assert.ok(killedText.includes("◼ stopped (SIGTERM)"), killedText);
  assert.equal(killedText.includes("✓ done"), false, killedText);
});

/**
 * A `!command` the user typed is labelled with the command itself, so the card printed
 * it twice — `ls -1 src ✓ done  bg_2 · ls -1 src · 0s`. Every field was right and the
 * row still read as a rendering bug.
 */
test("a job whose name IS its command shows the command once", () => {
  const now = 1_700_000_000_000;
  const job: JobView = {
    id: "bg_2",
    name: "ls -1 src",
    sessionId: "s1",
    pid: 11,
    command: "ls -1 src",
    status: "exited",
    exitCode: 0,
    exitedAt: now,
    startedAt: now - 400,
    tail: ["cart.py"],
    outputLines: 1,
  };
  const out: VLine[] = [];
  jobCardLines(out, job, 80, now);
  const text = joined(out);
  assert.equal(text.match(/ls -1 src/g)?.length, 1, text);
  // The id still shows: it is what `bashKill`/the rail address the job by.
  assert.ok(text.includes("bg_2"), text);

  // A named job with a DIFFERENT command still shows both — that is the normal case.
  const named: JobView = { ...job, name: "dev server", command: "npm run dev" };
  const out2: VLine[] = [];
  jobCardLines(out2, named, 80, now);
  const text2 = joined(out2);
  assert.ok(text2.includes("dev server") && text2.includes("npm run dev"), text2);
});

test("a [background] wake note is dropped while its job card shows it", () => {
  const note = '[background] bg_1 finished (exit 0) — command "make", 2 lines of output.';
  const thread = [msg({ id: "n1", role: "system", parts: [{ type: "text", text: note }] })];
  const job: JobView = {
    id: "bg_1",
    name: "make",
    sessionId: "s1",
    pid: 1,
    command: "make",
    status: "exited",
    exitCode: 0,
    startedAt: 1,
    exitedAt: 2,
  };
  const withCard = joined(buildLines(thread, CLOSED, CLOSED, 80, { jobs: [job], now: 3 }));
  assert.equal(withCard.includes("[background]"), false);
  assert.ok(withCard.includes("bg_1"));
  // Once the job ages out of the registry the note is all that is left — keep it.
  assert.ok(joined(buildLines(thread, CLOSED, CLOSED, 80)).includes("[background]"));
});

// ---- the viewport window ----------------------------------------------------

test("visibleSlice: pinned to the tail, scrolled up, and clamped past the top", () => {
  const lines: VLine[] = Array.from({ length: 100 }, (_v, i) => ({ text: `l${i}` }));
  const tail = visibleSlice(lines, 10, 0);
  assert.equal(tail.rows[0].text, "l90");
  assert.equal(tail.rows.at(-1)!.text, "l99");
  assert.equal(tail.more, 0);

  const up = visibleSlice(lines, 10, 20);
  assert.equal(up.start, 70);
  assert.equal(up.more, 20);

  // Fully scrolled up reads 0%: the percentage is the viewport TOP's position.
  const top = visibleSlice(lines, 10, 999);
  assert.equal(top.start, 0);
  assert.equal(top.more, 90);
  assert.equal(top.pct, 0);

  // A transcript shorter than the viewport shows everything and nothing below.
  const short = visibleSlice(lines.slice(0, 3), 10, 5);
  assert.equal(short.rows.length, 3);
  assert.equal(short.more, 0);
  assert.equal(short.start, 0);
});

// ---- click hit-testing ------------------------------------------------------
// The geometry these pin is shared with `Chat`'s render loop. A click that lands
// one row off its row is worse than one that does nothing, so the inverse is
// tested directly rather than through a mounted renderer.

test("chatBodyHeight subtracts the reserved strips", () => {
  // Two strips are always reserved (activity + scroll indicator), plus one per
  // queued message, plus one for a notice.
  assert.equal(chatBodyHeight(20, 0, false), 18);
  assert.equal(chatBodyHeight(20, 3, false), 15);
  assert.equal(chatBodyHeight(20, 0, true), 17);
  assert.equal(chatBodyHeight(20, 2, true), 15);
  // Never zero or negative, however little room there is.
  assert.equal(chatBodyHeight(1, 9, true), 1);
});

test("lineAtSlot inverts the pad — a short transcript hangs from the bottom", () => {
  const lines: VLine[] = [
    { text: "a", click: "ka" },
    { text: "b", click: "kb" },
  ];
  // Body of 5, two lines: three rows of empty air ABOVE, then the lines.
  assert.equal(lineAtSlot(lines, 5, 0, 0), null);
  assert.equal(lineAtSlot(lines, 5, 0, 2), null);
  assert.equal(lineAtSlot(lines, 5, 0, 3)?.click, "ka");
  assert.equal(lineAtSlot(lines, 5, 0, 4)?.click, "kb");
  // Off the bottom of the body.
  assert.equal(lineAtSlot(lines, 5, 0, 5), null);
});

test("lineAtSlot follows the scroll offset", () => {
  const lines: VLine[] = Array.from({ length: 10 }, (_, i) => ({
    text: `l${i}`,
    click: `k${i}`,
  }));
  // Pinned to the tail: a body of 3 shows the last three, no pad.
  assert.equal(lineAtSlot(lines, 3, 0, 0)?.click, "k7");
  assert.equal(lineAtSlot(lines, 3, 0, 2)?.click, "k9");
  // Scrolled back two: the same slots resolve two lines earlier.
  assert.equal(lineAtSlot(lines, 3, 2, 0)?.click, "k5");
  assert.equal(lineAtSlot(lines, 3, 2, 2)?.click, "k7");
});

test("a subagent card is clickable and descends rather than folds", () => {
  const out: VLine[] = [];
  const branch: Branch = {
    id: "sub_9",
    title: "explore the parser",
    busy: false,
    status: "done",
  };
  buildLines([], () => false, () => false, 80, { branches: [branch] })
    .forEach((l) => out.push(l));
  const card = out.find((l) => l.click === "open:sub_9");
  // The target exists AND it is the descend form, not a fold key — the dispatcher
  // branches on the `open:` prefix, so the prefix is the contract.
  assert.ok(card, "the branch card carries an open: click target");
});

/**
 * Loading a skill was invisible. The named skill's whole SKILL.md goes into that
 * turn's prompt, and the transcript said nothing — so a typo'd name looked exactly
 * like a working one, and the only sign a skill was involved at all came from the
 * model when the file was broken.
 */
test("a message that names an installed skill says so, under the message", () => {
  const installed = ["prewalk", "exa", "shell-use"];
  assert.deepEqual(skillsNamed("/prewalk fix the parser", installed), ["prewalk"]);
  // Mid-sentence counts: a skill reference is text the model reads, wherever it sits.
  assert.deepEqual(skillsNamed("fix this, use /exa to check", installed), ["exa"]);
  assert.deepEqual(skillsNamed("/exa and /shell-use please", installed), ["exa", "shell-use"]);
  // Names not installed claim nothing — the row must never invent a skill.
  assert.deepEqual(skillsNamed("/model", installed), []);
  assert.deepEqual(skillsNamed("/prewalkk typo", installed), []);
  // A path is not a skill reference.
  assert.deepEqual(skillsNamed("look at src/exa/mod.ts", installed), []);
  assert.deepEqual(skillsNamed("/prewalk", []), []);
  // Repeats collapse.
  assert.deepEqual(skillsNamed("/exa then /exa again", installed), ["exa"]);

  const thread = [msg({ id: "u1", role: "user", parts: [{ type: "text", text: "/exa search" }] })];
  const text = joined(buildLines(thread, () => false, () => false, 80, { skills: installed }));
  assert.match(text, /↳ skill loaded: \/exa/);
  // With no skills fetched yet, no claim is made.
  const bare = joined(buildLines(thread, () => false, () => false, 80, {}));
  assert.equal(bare.includes("skill loaded"), false);
});

/**
 * Job cards used to be appended after the WHOLE thread, so a `!python3 tests/…` that
 * failed three turns ago sat permanently below the newest reply, still showing its
 * traceback after the bug had been fixed — the most recent thing on screen was the
 * oldest news. Seen in a multi-turn soak.
 */
test("an exited job card lands where the command finished, not at the tail", () => {
  const job = (id: string, exitedAt: number, code: number): JobView => ({
    id,
    name: id,
    sessionId: "s1",
    pid: 1,
    command: `run ${id}`,
    status: "exited",
    exitCode: code,
    startedAt: exitedAt - 100,
    exitedAt,
    tail: [`${id}-output`],
    outputLines: 1,
  });
  const thread = [
    msg({ id: "u1", role: "user", parts: [{ type: "text", text: "first" }], createdAt: 10 }),
    msg({ id: "a1", role: "supervisor", parts: [{ type: "text", text: "answer one" }], createdAt: 20 }),
    msg({ id: "u2", role: "user", parts: [{ type: "text", text: "second" }], createdAt: 40 }),
    msg({ id: "a2", role: "supervisor", parts: [{ type: "text", text: "answer two" }], createdAt: 50 }),
  ];
  const out = buildLines(thread, () => false, () => false, 80, {
    jobs: [job("bg_1", 30, 1)],
    now: 100,
  });
  const text = joined(out);
  // Between the two exchanges, in time order.
  assert.ok(text.indexOf("bg_1-output") > text.indexOf("answer one"), text);
  assert.ok(text.indexOf("bg_1-output") < text.indexOf("second"), text);

  // A job that exited after the last message stays at the bottom — there is nothing
  // later to place it before.
  const late = buildLines(thread, () => false, () => false, 80, {
    jobs: [job("bg_2", 90, 0)],
    now: 100,
  });
  const lateText = joined(late);
  assert.ok(lateText.indexOf("bg_2-output") > lateText.indexOf("answer two"), lateText);

  // A RUNNING job also sits at the bottom: it has no exit time to place it by, and it
  // is the one card whose value is being next to the composer.
  const running = buildLines(thread, () => false, () => false, 80, {
    jobs: [{ ...job("bg_3", 0, 0), status: "running", exitedAt: undefined, tail: ["live"] }],
    now: 100,
  });
  assert.ok(joined(running).indexOf("live") > joined(running).indexOf("answer two"));
});

/**
 * A four-round turn of shell calls headlined as `4 steps · ran 1 command · ran 1 command
 * · ran 1 command · ran 1 command` — the same three words four times, filling the row
 * that is supposed to say what the turn did.
 */
/**
 * A call that is not the program tool is the model reaching for a host function AS a tool. Saying
 * so beats printing the arguments: on a fresh walk one such row read
 * `✗ error  1 step · {"input":"[/private/tmp/claude-501/-Users-andrey…`.
 */
test("a host function called as a tool is named, not dumped as JSON", () => {
  const parts: Part[] = [
    { type: "tool_call", id: "p1", name: "patch", input: { input: "[src/cart.py#a1]\nDEL 2.=3" } },
    {
      type: "tool_result",
      callId: "p1",
      output: "patch is a host function, not a tool",
      isError: true,
    } as Part,
  ];
  const text = joined(
    buildLines([msg({ id: "a1", role: "supervisor", parts })], () => false, () => false, 100, {}),
  );
  assert.match(text, /called patch as a tool/);
  assert.equal(text.includes('{"input"'), false, text);
});

test("a collapsed step's repeated summaries collapse to a count", () => {
  const parts: Part[] = [];
  for (let i = 1; i <= 4; i++) {
    parts.push({ type: "tool_call", id: `c${i}`, name: "run_steps", input: { code: 'await bash("ls");' } });
    parts.push({ type: "tool_result", callId: `c${i}`, output: "ok", isError: false } as Part);
  }
  const thread = [msg({ id: "a1", role: "supervisor", parts })];
  const text = joined(buildLines(thread, () => false, () => false, 100, {}));
  assert.match(text, /4 steps/);
  assert.match(text, /ran 1 command ×4/);
  // The un-collapsed repetition is what this replaces.
  assert.equal(/ran 1 command · ran 1 command/.test(text), false, text);

  // Different summaries are still listed separately — the collapse must not merge
  // unlike steps into one claim.
  const mixed: Part[] = [
    { type: "tool_call", id: "d1", name: "run_steps", input: { code: 'await bash("ls");' } },
    { type: "tool_result", callId: "d1", output: "ok", isError: false } as Part,
    { type: "tool_call", id: "d2", name: "run_steps", input: { code: 'await write("a.ts", x);' } },
    { type: "tool_result", callId: "d2", output: "ok", isError: false } as Part,
  ];
  const mixedText = joined(
    buildLines([msg({ id: "a2", role: "supervisor", parts: mixed })], () => false, () => false, 100, {}),
  );
  // Unlike steps are listed separately — and now on separate ROWS, one per thing that
  // happened, rather than packed onto the header and clipped at the right edge.
  assert.match(mixedText, /ran 1 command\n/);
  assert.match(mixedText, /wrote a\.ts/);
  assert.equal(/ran 1 command · wrote a\.ts/.test(mixedText), false, mixedText);
});

/**
 * The mapping behind the delegated-report card, which had never rendered at all because
 * nothing passed `branches` to `buildLines`. Asserted HERE rather than through the render
 * harness: the version of this test that went through `App` passed while the bug was
 * still live, because it matched text the raw note also contains.
 */
test("branchesFrom pairs a spawned child with its report note", () => {
  const noteText = [
    '[subagent finished] "create mango.py" (agent-1) — finished.',
    "Changed files: src/mango.py.",
    "Report:",
    "Created src/mango.py with a mango() function.",
    "It worked in THIS session's checkout, so its edits are already here — read them.",
  ].join("\n");
  const thread = [
    msg({ id: "u1", role: "user", parts: [{ type: "text", text: "spawn one" }] }),
    msg({ id: "n1", role: "system", parts: [{ type: "text", text: noteText }] }),
  ];
  const child = {
    id: "agent-1",
    title: "subagent · create mango.py",
    kind: "subagent" as const,
    busy: false,
    lastTurnStatus: "done",
    outcomeOk: true,
    originMessageId: "u1",
  };

  const [b] = branchesFrom(thread, [child]);
  assert.equal(b.id, "agent-1");
  assert.equal(b.busy, false);
  assert.equal(b.status, "done");
  assert.equal(b.ok, true);
  // Where the card is drawn — the note does not carry this, the child does.
  assert.equal(b.originMessageId, "u1");
  assert.equal(b.note?.sessionId, "agent-1");
  assert.deepEqual(b.note?.files, ["src/mango.py"]);

  // AND the note it paired is then dropped from the raw thread, so the reader sees the
  // card instead of the prose written for the model.
  const text = joined(buildLines(thread, () => false, () => false, 100, { branches: [b] }));
  assert.ok(text.includes("create mango.py"), text);
  assert.equal(text.includes("It worked in THIS session"), false, text);
  assert.equal(text.includes("[subagent finished]"), false, text);
});

test("branchesFrom reports a running child as running, and leaves an unpaired note raw", () => {
  const noteText = '[subagent finished] "other" (agent-9) — finished.\nChanged files: none.';
  const thread = [msg({ id: "n1", role: "system", parts: [{ type: "text", text: noteText }] })];

  // `running` is not a settled status and must not be reported as one.
  const [live] = branchesFrom([], [{ id: "a", title: "t", kind: "subagent", busy: true, lastTurnStatus: "running" }]);
  assert.equal(live.status, undefined);
  assert.equal(live.busy, true);
  assert.equal(live.ok, undefined);

  // A note whose session is not among the children yields no branch — so `buildLines`
  // keeps the raw note, and the report reaches the reader rather than vanishing.
  assert.deepEqual(branchesFrom(thread, []), []);

  // DELEGATED KINDS ONLY. `originId` also holds forks, compactions and handoffs, so a
  // handoff of this conversation rendered as `◆ handoff · … ✓ done` — a sibling
  // conversation dressed as a subagent's report. Caught by switching conversations while
  // a fan-out ran, which is also how the ask leak was caught.
  const sibling = { id: "hand-1", title: "handoff · x", busy: false, lastTurnStatus: "done" };
  assert.deepEqual(branchesFrom([], [{ ...sibling, kind: "root" }]), []);
  assert.deepEqual(branchesFrom([], [{ ...sibling, kind: "fork" }]), []);
  assert.deepEqual(branchesFrom([], [{ ...sibling, kind: "compaction" }]), []);
  assert.equal(branchesFrom([], [{ ...sibling, kind: "workflow_agent" }]).length, 1);
  const text = joined(buildLines(thread, () => false, () => false, 100, { branches: [] }));
  assert.ok(text.includes("[subagent finished]"), text);
});

test("several steps are several rows, one step stays on the header", () => {
  // Asked for: collapsed tool calls printed on their own rows instead of packed onto one.
  // Four different operations used to read
  //   ▸ 4 steps · read a.ts · wrote b.ts · ran 1 command · started 1 background command
  // — clipped at the right edge, with the last one silently gone.
  const many: Part[] = [];
  const codes = [
    'await view("a.ts");',
    'await write("b.ts", x);',
    'await bash("ls");',
    'await bashBg("npm run dev");',
  ];
  codes.forEach((code, i) => {
    many.push({ type: "tool_call", id: `c${i}`, name: "run_steps", input: { code } });
    many.push({ type: "tool_result", callId: `c${i}`, output: "ok", isError: false } as Part);
  });
  const rows = messageLines(msg({ id: "m1", parts: many }), CLOSED, CLOSED, 100).map((l) =>
    l.text
  );
  assert.ok(rows.some((t) => t.includes("4 steps")), rows.join("\n"));
  for (const gist of ["read a.ts", "wrote b.ts", "ran 1 command", "started 1 background"]) {
    assert.equal(
      rows.filter((t) => t.includes(gist)).length,
      1,
      `${gist} should be on exactly one row of:\n${rows.join("\n")}`,
    );
  }
  // Every row opens the same fold — a list whose rows are not all click targets is worse
  // than the packed header it replaces.
  const keys = new Set(messageLines(msg({ id: "m1", parts: many }), CLOSED, CLOSED, 100)
    .map((l) => l.click).filter(Boolean));
  assert.equal(keys.size, 1, [...keys].join(","));

  // ONE step still shares the header: `1 step` over a single indented line is a row spent
  // saying "one".
  const one = messageLines(
    msg({
      id: "m2",
      parts: [
        { type: "tool_call", id: "x", name: "run_steps", input: { code: 'await view("a.ts");' } },
        { type: "tool_result", callId: "x", output: "ok", isError: false } as Part,
      ],
    }),
    CLOSED,
    CLOSED,
    100,
    // `messageLines` also emits the role row, which is not a step.
  ).map((l) => l.text).filter((t) => t.trim() && !t.includes("bough"));
  assert.equal(one.length, 1, one.join("\n"));
  assert.ok(one[0].includes("1 step") && one[0].includes("read a.ts"), one[0]);
});

test("an expanded call is labelled by what it did, not by the tool's name", () => {
  // bough grants ONE program tool, so an expanded four-call turn read `◇ run_steps ✓ done`
  // four times — the same internal identifier, directly under a collapsed list that had just
  // named each step. Expanding should keep the label you clicked on.
  const parts: Part[] = [
    { type: "tool_call", id: "c1", name: "run_steps", input: { code: 'await view("a.ts");' } },
    { type: "tool_result", callId: "c1", output: "ok", isError: false } as Part,
    { type: "tool_call", id: "c2", name: "run_steps", input: { code: 'await bash("ls");' } },
    { type: "tool_result", callId: "c2", output: "ok", isError: false } as Part,
  ];
  const open = () => true;
  const rows = messageLines(msg({ id: "m1", parts }), open, () => false, 100).map((l) => l.text);
  assert.ok(rows.some((t) => t.includes("read a.ts") && t.includes("✓ done")), rows.join("\n"));
  assert.ok(rows.some((t) => t.includes("ran 1 command") && t.includes("✓ done")), rows.join("\n"));
  assert.equal(
    rows.filter((t) => t.includes("◇") && t.includes("run_steps")).length,
    0,
    rows.join("\n"),
  );

  // A name the model INVENTED is still shown — it is the only thing identifying that call —
  // but marked as what it was: a host function reached for as a tool.
  const invented: Part[] = [
    { type: "tool_call", id: "d1", name: "patch", input: { path: "a.ts" } },
    { type: "tool_result", callId: "d1", output: "unknown tool", isError: true } as Part,
  ];
  const bad = messageLines(msg({ id: "m2", parts: invented }), open, () => false, 100)
    .map((l) => l.text);
  assert.ok(bad.some((t) => t.includes("patch (as a tool)")), bad.join("\n"));
});

test("a command that exited non-zero is flagged on the collapsed row", () => {
  // A reviewer persona read `▸ 3 steps · wrote cart.py · ran 1 command · ran 1 command` and
  // had to expand to learn one of those commands exited 127. `bash()` reports a non-zero exit
  // as DATA, so the round itself is a legitimate `✓ done` — the row has to carry the failure.
  const m = msg({
    id: "m1",
    parts: [
      call("c1", 'await bash("python x.py");'),
      result("c1", "/bin/sh: python: command not found\n[exit code 127]"),
    ],
  });
  const head = messageLines(m, CLOSED, CLOSED, 120).find((l) => l.text.includes("step"))!;
  assert.ok(head.text.includes("1 command failed"), head.text);
  // Not an ERROR: the program ran fine and the exit code is a result it was handed.
  assert.equal(head.text.includes("✗ error"), false, head.text);

  // A clean command says nothing extra.
  const ok = msg({
    id: "m2",
    parts: [call("c1", 'await bash("true");'), result("c1", "done")],
  });
  const okHead = messageLines(ok, CLOSED, CLOSED, 120).find((l) => l.text.includes("step"))!;
  assert.equal(okHead.text.includes("failed"), false, okHead.text);
});

// ---- the memory margin (`#` rows) -------------------------------------------

test("primed tags render once, dim, as the transcript's first row", () => {
  const thread = [msg({ id: "m1", role: "user", parts: [{ type: "text", text: "hi" }] })];
  const lines = buildLines(thread, OPEN, OPEN, 100, {
    primedTags: ["git:push", "bun:test", "psql:migrate"],
  });
  assert.equal(lines[0].text, "# this repo remembers: git:push · bun:test · psql:migrate");
  assert.equal(lines[0].copy, lines[0].text);
  // No primed tags — no row, not an empty row.
  const bare = buildLines(thread, OPEN, OPEN, 100);
  assert.ok(!bare[0].text.startsWith("#"));
});

test("a primed row longer than the terminal truncates with an ellipsis", () => {
  const lines = buildLines([], OPEN, OPEN, 40, {
    primedTags: ["git:push", "bun:test", "psql:migrate", "docker:exec"],
  });
  assert.ok(lines[0].text.endsWith("…"), lines[0].text);
  assert.ok(width(lines[0].text) <= 40, lines[0].text);
});

test("the injected AGENTS.md files render as their own # row, under the tags one", () => {
  const thread = [msg({ id: "m1", role: "user", parts: [{ type: "text", text: "hi" }] })];
  const lines = buildLines(thread, OPEN, OPEN, 100, {
    primedTags: ["git:push"],
    projectRules: [{ label: "AGENTS.md" }, { label: "packages/api/AGENTS.md" }],
  });
  assert.equal(lines[0].text, "# this repo remembers: git:push");
  // Names the files and points at the command that prints them in full — the row is
  // one line, and "which rules am I under" deserves a longer answer than fits here.
  assert.equal(
    lines[1].text,
    "# rules: AGENTS.md · packages/api/AGENTS.md · /rules",
  );
  // No AGENTS.md anywhere — no row, exactly as with no primed tags.
  const bare = buildLines(thread, OPEN, OPEN, 100, { primedTags: ["git:push"] });
  assert.equal(bare[1].text.startsWith("#"), false);
});

test("a rules row that fills the terminal drops its hint rather than wrapping", () => {
  const lines = buildLines([], OPEN, OPEN, 40, {
    projectRules: [
      { label: "AGENTS.md" },
      { label: "packages/api/AGENTS.md" },
      { label: "packages/web/AGENTS.md" },
    ],
  });
  assert.ok(width(lines[0].text) <= 40, lines[0].text);
  // The suffix is an advertisement; a row that has already been elided has no room
  // for one, and half a `/rules` reads as corruption.
  assert.equal(lines[0].text.includes("/rules"), false, lines[0].text);
});

test("[history] hints leave the output block and become # marginalia", () => {
  const out =
    "ok\n[history] tags previously used in migrations/: psql, alembic — see history.sql() for the commands behind them";
  const m = msg({ id: "m1", parts: [call("c1", "await bash('x', 'y')"), result("c1", out)] });
  const lines = buildLines([m], OPEN, OPEN, 100, {});
  const text = joined(lines);
  // The hint line is rewritten and outside the │ block…
  assert.ok(text.includes("  # migrations/ also remembers: psql · alembic"), text);
  // …and the model-facing raw line is nowhere on screen.
  assert.ok(!text.includes("[history]"), text);
  assert.ok(!text.includes("history.sql()"), text);
  // The block keeps the program's real output.
  assert.ok(text.includes("│ ok"), text);
});

test("a result that is ONLY hints renders no output block at all", () => {
  const out = "[history] tags previously used in src/tui/: opentui — see history.sql()";
  const m = msg({ id: "m1", parts: [call("c1", "await view('a')"), result("c1", out)] });
  const text = joined(buildLines([m], OPEN, OPEN, 100, {}));
  assert.ok(!text.includes("↳ output"), text);
  assert.ok(text.includes("# src/tui/ also remembers: opentui"), text);
});
