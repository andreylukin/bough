import assert from "node:assert/strict";
import {
  type Branch,
  buildLines,
  jobCardLines,
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

Deno.test("every emitted line fits the width — long prose, long code, long output", () => {
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

Deno.test("a hard-wrapped block keeps its gutter on every physical line", () => {
  const m = msg({
    id: "m1",
    parts: [call("c1", "const x = 1"), result("c1", "z".repeat(120))],
  });
  const out = messageLines(m, OPEN, CLOSED, 40).filter((l) => l.text.includes("z"));
  assert.ok(out.length >= 3); // 120 columns of output across a ~36-column gutter
  for (const l of out) assert.ok(l.text.trimStart().startsWith("│"), l.text);
});

// ---- folding: the program that ran ------------------------------------------

Deno.test("a collapsed tool step carries the gist of the program that ran", () => {
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
  assert.ok(head.text.includes("run_steps"));
  assert.ok(head.text.includes("await bash('deno test')"), head.text);
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

Deno.test("one round of program + result is ONE step, not two entries", () => {
  const m = msg({
    id: "m1",
    parts: [call("c1", "a()"), result("c1", "1"), call("c2", "b()"), result("c2", "2")],
  });
  const heads = messageLines(m, CLOSED, CLOSED, 120).filter((l) => l.text.includes("steps"));
  assert.equal(heads.length, 1);
  assert.ok(heads[0].text.includes("2 steps"));
});

Deno.test("a running call is visible on the collapsed header and shows live console output", () => {
  const m = msg({
    id: "m1",
    pending: true,
    parts: [call("c1", "console.log('x')")],
  });
  const logs = { c1: ["first", "second"] };
  const head = messageLines(m, CLOSED, CLOSED, 120, undefined, logs)
    .find((l) => l.text.includes("step"))!;
  assert.ok(head.text.includes("⚙ run_steps…"), head.text);

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

Deno.test("caps: a long program and a long output truncate; only !full lifts them", () => {
  const code = Array.from({ length: 40 }, (_v, i) => `line ${i}`).join("\n");
  const out = Array.from({ length: 60 }, (_v, i) => `out ${i}`).join("\n");
  const m = msg({ id: "m1", parts: [call("c1", code), result("c1", out)] });

  const capped = joined(messageLines(m, OPEN, CLOSED, 120));
  assert.ok(capped.includes("line 13")); // CODE_LINES = 14
  assert.equal(capped.includes("line 14"), false);
  assert.ok(capped.includes("out 19")); // OUTPUT_LINES = 20
  assert.equal(capped.includes("out 20"), false);
  assert.ok(capped.includes("more lines · click to show all"));

  // The cap-lift is its own key, so expand-all cannot dump the whole thing.
  const more = messageLines(m, OPEN, CLOSED, 120).find((l) => l.text.includes("more lines"))!;
  assert.equal(more.click, "m1:0!full");
  const full = joined(messageLines(m, OPEN, () => true, 120));
  assert.ok(full.includes("line 39") && full.includes("out 59"));
  assert.equal(full.includes("more lines · click to show all"), false);
});

Deno.test("an interrupted result keeps its partial output and never reads ✓ done", () => {
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

Deno.test("an errored result is marked on the closed header", () => {
  const m = msg({
    id: "m1",
    parts: [call("c1", "boom()"), result("c1", "[program error] boom", { isError: true })],
  });
  const head = messageLines(m, CLOSED, CLOSED, 120).find((l) => l.text.includes("step"))!;
  assert.ok(head.text.includes("✗ error"), head.text);
});

// ---- folding: reasoning -----------------------------------------------------

Deno.test("reasoning folds: a collapsed gist line, an expanded gutter block", () => {
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

Deno.test("reasoning with no text renders nothing at all", () => {
  const m = msg({
    id: "m1",
    parts: [{ type: "reasoning", text: "  \n " }, { type: "text", text: "hi" }],
  });
  assert.equal(joined(messageLines(m, CLOSED, CLOSED, 120)).includes("thinking"), false);
});

// ---- other part kinds -------------------------------------------------------

Deno.test("a settled ask renders as one always-visible Q → A line", () => {
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

Deno.test("an image part renders as a compact placeholder and copies its path", () => {
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

Deno.test("an [image] system note collapses onto the placeholder — no role label, no path", () => {
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

// ---- system-note parsing ----------------------------------------------------

const NOTE = (status: string, files: string, report: string | null) =>
  [
    `[subagent finished] "extract token logic" (sub-1) — ${status}.`,
    `Changed files: ${files}.`,
    report ? `Report:\n${report}` : "No report.",
    "It worked in THIS session's checkout, so its edits are already here — read them " +
    "before building on top; there is nothing to merge.",
  ].join("\n");

Deno.test("parseSubagentNote: the real note shape from agents/notes.ts", () => {
  const p = parseSubagentNote(NOTE("finished", "a.ts, b.ts", "# Findings\nAll good."))!;
  assert.equal(p.title, "extract token logic");
  assert.equal(p.sessionId, "sub-1");
  assert.equal(p.ok, true);
  assert.deepEqual(p.files, ["a.ts", "b.ts"]);
  assert.equal(p.report, "# Findings\nAll good.");
});

Deno.test("parseSubagentNote: the four outcomes stay distinguishable", () => {
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

Deno.test("parseSubagentNote: 'not reported' is unknown, not a file named so", () => {
  const p = parseSubagentNote(NOTE("finished", "not reported", null))!;
  assert.deepEqual(p.files, []);
  assert.equal(p.filesUnknown, true);
});

Deno.test("parseBgNote / parseImageNote", () => {
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

Deno.test("a finished subagent's card replaces its raw note at the spawn point", () => {
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
  assert.ok(text.includes("its edits are already in this checkout"));

  // With no branch to draw the card, the note itself must survive — otherwise the
  // report would render nowhere at all.
  const bare = joined(buildLines(thread, CLOSED, CLOSED, 100));
  assert.ok(bare.includes("[subagent finished]"));
});

Deno.test("a running subagent is left to the rail; a card with no spawn point tails out", () => {
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

Deno.test("branch cards state the real outcome — failed and orphaned never read done", () => {
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
});

Deno.test("a message steered into a running turn carries a queued ack until a reply follows", () => {
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

Deno.test("job cards: a running shell looks alive, an exited one states its outcome", () => {
  const now = 100_000;
  const running: JobView = {
    id: "bg_1",
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
  assert.ok(out.every((l) => l.click === "jobs" || l.text.trim() === ""));
});

Deno.test("a [background] wake note is dropped while its job card shows it", () => {
  const note = '[background] bg_1 finished (exit 0) — command "make", 2 lines of output.';
  const thread = [msg({ id: "n1", role: "system", parts: [{ type: "text", text: note }] })];
  const job: JobView = {
    id: "bg_1",
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

Deno.test("visibleSlice: pinned to the tail, scrolled up, and clamped past the top", () => {
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
