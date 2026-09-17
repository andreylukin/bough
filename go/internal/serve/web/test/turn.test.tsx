import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
// No DOM under bun: markdown sanitising is not what these assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { Entry, TurnView, swallowedByStop } = await import("../src/app");
const { groupTurns } = await import("../src/render");
import type { Line } from "../src/types";

const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const code = 'tools.bash("go test ./...")';
const live: Line[] = [
  { seq: 1, at: at(0), kind: "input", text: "Make the tests fast." },
  { seq: 2, at: at(1), kind: "thinking", text: "The suite sleeps." },
  { seq: 3, at: at(15), kind: "code", text: code },
];

test("a running turn never renders a done footer", () => {
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(live)[0]} />);
  expect(html).not.toContain("turn-foot");
});

test("a finished turn ends on outcome, worked-for and a files chip", () => {
  const done: Line[] = [...live, { seq: 4, at: at(15), kind: "result", text: code + "\nok", data: { code, exit: 0 } },
    { seq: 5, at: at(20), kind: "done", text: "", data: { files: ["a.go", "b.go"], usage: { in: 1000, out: 100, cost: 0.02 } } }];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(done)[0]} />);
  expect(html).toContain("turn-foot");
  expect(html).toContain("Worked for 20s");
  expect(html).toContain("2 files");
  expect(html).toMatch(/title="[^"]* in \/ [^"]* out"/);
});

test("recorded thinking reads Thought for Ns", () => {
  expect(renderToStaticMarkup(<Entry line={live[1]} codes={[]} until={at(15)} />)).toContain("Thought for 14s");
});

test("a turn cut off by a later one stops its open rows and says Interrupted", () => {
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(live)[0]} superseded />);
  expect(html).toContain("Interrupted · result not recorded");
  expect(html).toContain("turn-foot");
  expect(renderToStaticMarkup(<TurnView turn={groupTurns(live)[0]} />)).not.toContain("result not recorded");
});

test("a steer input after a still-open turn does not cut it off", () => {
  const steer: Line = { seq: 99, kind: "input", text: "also check vet", at: at(20) } as Line;
  const turns = groupTurns([...live, steer]);
  const html = renderToStaticMarkup(<TurnView turn={turns[0]} superseded={turns[0].prompt !== null && turns.slice(1).some((u) => u.done)} />);
  expect(html).not.toContain("result not recorded");
});

test("Stop rescues only rows unsent at Stop, once the turn ended after them", () => {
  const p = { id: "a", text: "hi", after: 5 };
  const done: Line = { seq: 6, kind: "cancelled", at: at(1) } as Line;
  expect(swallowedByStop([p], new Set(), [done])).toEqual([]);
  expect(swallowedByStop([p], new Set(["a"]), [])).toEqual([]);
  expect(swallowedByStop([p], new Set(["a"]), [done])).toEqual([p]);
  expect(swallowedByStop([p], new Set(["a"]), [done, { seq: 7, kind: "input", text: "hi", at: at(2) } as Line])).toEqual([]);
});

// R2-C: Esc mid-stream keeps the partial answer above the Stopped footer.
test("a turn stopped mid-answer shows the partial text and Stopped", () => {
  const lines: Line[] = [
    { seq: 1, at: at(0), kind: "input", text: "Explain the loop." },
    { seq: 2, at: at(5), kind: "assistant", text: "The loop reads input and streams", data: { partial: true } },
    { seq: 3, at: at(5), kind: "cancelled", text: "" },
    { seq: 4, at: at(5), kind: "done", text: "" },
  ] as Line[];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).toContain("The loop reads input and streams");
  expect(html).toContain("Stopped");
  expect(html.indexOf("The loop reads input")).toBeLessThan(html.indexOf("turn-foot"));
});

// WEB2: tool rows say what happened.
const { ToolCall, ToolRun } = await import("../src/app");
const { splitWork: split, groupTools } = await import("../src/render");

test("a program that threw reads failed with its error, not exit 0", () => {
  const c = 'console.log(tools.bash("ls -a"))\nconsole.log(tools.view("package.json"))';
  const html = renderToStaticMarkup(<ToolCall code={{ seq: 1, at: at(0), kind: "code", text: c }}
    result={{ seq: 2, at: at(1), kind: "result", text: ".\nsrc\nerror: GoError: open package.json: no such file or directory\n\n[the 1 code block(s) after this one in your reply were not run: this block failed.]", data: { code: c, exit: 0, ms: 10 } }} />);
  expect(html).toContain("block-failed");
  expect(html).toContain("open package.json");
  expect(html).not.toContain("exit 0");
});

test("a recorded error field marks the call failed", () => {
  const c = 'console.log(tools.view("x"))';
  const html = renderToStaticMarkup(<ToolCall code={{ seq: 1, at: at(0), kind: "code", text: c }}
    result={{ seq: 2, at: at(1), kind: "result", text: "error: patch: old text not found in x\nnear line 3", data: { code: c, error: "patch: old text not found in x\nnear line 3" } }} />);
  expect(html).toContain("block-failed");
  expect(html).toContain("patch: old text not found in x");
});

const edits: Line[] = [
  { seq: 1, at: at(0), kind: "code", text: 'console.log(tools.bash("ls -a"))\nconsole.log(tools.view("src/math.ts"))' },
  { seq: 2, at: at(1), kind: "result", text: ".\nsrc", data: { exit: 0, ms: 10 } },
  { seq: 3, at: at(2), kind: "code", text: 'console.log(tools.patch("src/math.ts", "a", "a\\nb"))\nconsole.log(tools.patch("src/index.ts", "c", "d\\ne"))' },
  { seq: 4, at: at(3), kind: "result", text: "patched src/math.ts (+1 lines)\n\n a\n+b\npatched src/index.ts (+1 lines)\n\n-c\n+d\n+e\n[lsp] no errors", data: { ms: 900 } },
  { seq: 5, at: at(4), kind: "code", text: 'console.log(tools.bash("tsc --noEmit"))' },
  { seq: 6, at: at(5), kind: "result", text: "", data: { exit: 0, ms: 600 } },
];

test("a mixed fold is named by its edits, never by its first command", () => {
  const html = renderToStaticMarkup(<ToolRun lines={edits} codes={[]} />);
  expect(html).not.toContain("Tool group");
  expect(html).toContain("Edited math.ts, index.ts");
  expect(html).toContain("+3</span>");
  expect(html).toContain("−1</span>");
  expect(html).toContain("read 1 file · ran 2 commands");
});

test("an edit's output renders as a diff with add and del lines", () => {
  const html = renderToStaticMarkup(<ToolCall code={edits[2]} result={edits[3]} />);
  expect(html).toContain("edit-file");
  expect(html).toContain("dl-add");
  expect(html).toContain("dl-del");
  expect(html).toContain("[lsp] no errors");
});

test("a background agent's finish notice in a stopped turn is its own row, not work", () => {
  const ls: Line[] = [
    { seq: 1, at: at(0), kind: "input", text: "tick" },
    { seq: 2, at: at(1), kind: "code", text: 'tools.bash("sleep 1")' },
    { seq: 3, at: at(2), kind: "result", text: "", data: { exit: 0 } },
    { seq: 4, at: at(3), kind: "job", text: "[agent List files · 01a0 finished] Wrote COUNTS.md" },
    { seq: 5, at: at(4), kind: "cancelled", text: "" },
    { seq: 6, at: at(4), kind: "done", text: "", data: { exit: 0 } },
  ];
  const turn = groupTurns(ls)[0];
  const segs = split(groupTools(turn.body.map((l) => ({ kind: "line", seq: l.seq, line: l }) as never), []), [], false);
  expect(segs.some((s) => s.kind === "notice")).toBe(true);
  const html = renderToStaticMarkup(<TurnView turn={turn} />);
  expect(html).toContain("Background agent finished");
  expect(html).not.toContain("[agent List files");
});

test("a prompt offers Copy and Edit into composer; an answer offers Copy as Markdown and its time", () => {
  const lines: Line[] = [...live.slice(0, 1), { seq: 2, at: at(3), kind: "assistant", text: "Use **fake** clocks." }];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).toContain('aria-label="Copy prompt"');
  expect(html).toContain("Edit into composer");
  expect(html).toContain('aria-label="Copy answer as Markdown"');
  expect(html).toContain("say-time");
});

// R2-D: failed turns, wake copy, command-only turns.
test("a turn that ended on an error says Failed, not Done", () => {
  const ls: Line[] = [...live.slice(0, 1),
    { seq: 2, at: at(1), kind: "error", text: "401 Unauthorized" },
    { seq: 3, at: at(2), kind: "done", text: "", data: {} }];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(ls)[0]} />);
  expect(html).toContain("401 Unauthorized");
  expect(html).not.toContain(">Done<");
});

test("a failed retry says Failed; a retry that recovered says Done", () => {
  const base: Line[] = [...live.slice(0, 1),
    { seq: 2, at: at(1), kind: "error", text: "429" },
    { seq: 3, at: at(2), kind: "system", text: "provider hiccup — retrying in 2s, attempt 2 of 3" }];
  const failed = [...base, { seq: 4, at: at(3), kind: "error", text: "429" }, { seq: 5, at: at(4), kind: "done", text: "", data: {} }];
  expect(renderToStaticMarkup(<TurnView turn={groupTurns(failed)[0]} />)).not.toContain(">Done<");
  const ok = [...base, { seq: 4, at: at(3), kind: "assistant", text: "Fixed." }, { seq: 5, at: at(4), kind: "done", text: "", data: {} }];
  expect(renderToStaticMarkup(<TurnView turn={groupTurns(ok)[0]} />)).toContain(">Done<");
});

test("an agent wake-up says a background agent finished, never 0 jobs", () => {
  const wake = "[background job] A command you started in the background has finished while you were idle. Deal with it if it needs anything, then reply to the user with what happened.\n\n[agent List files · 01a0 finished] Wrote COUNTS.md";
  const html = renderToStaticMarkup(<TurnView turn={groupTurns([{ seq: 1, at: at(0), kind: "input", text: wake }])[0]} />);
  expect(html).not.toContain("0 background jobs");
  expect(html).toContain("A background agent finished");
  expect(html).not.toContain("[background job]");
});

test("a command-only turn is not Interrupted", () => {
  const ls: Line[] = [{ seq: 1, at: at(0), kind: "command", text: "/model opus" }, { seq: 2, at: at(0), kind: "system", text: "model: opus" }];
  expect(renderToStaticMarkup(<TurnView turn={groupTurns(ls)[0]} superseded />)).not.toContain("Interrupted");
});

test("a wake-up from a job and an agent shows both", () => {
  const wake = "[background job] A command you started in the background has finished while you were idle.\n\njob 3 [exited 0] make (1s)\n[agent Lint · 02b1 finished] clean";
  const html = renderToStaticMarkup(<TurnView turn={groupTurns([{ seq: 1, at: at(0), kind: "input", text: wake }])[0]} />);
  expect(html).toContain("A background agent finished");
  expect(html).toContain("A background job finished");
});

// R2-G: a program that edits reads "Edited N files +a −d", writes included, with one row per file.
test("an editing program is headed by every file it changed", () => {
  const c = 'console.log(tools.patch("src/math.js", "a", "a\\nb"))\ntools.write("src/main.js", "x\\ny\\n")\nconsole.log(tools.bash("node src/main.js"))';
  const html = renderToStaticMarkup(<ToolCall code={{ seq: 1, at: at(0), kind: "code", text: c }}
    result={{ seq: 2, at: at(1), kind: "result", text: "patched src/math.js (+1 lines)\n\n a\n+b\n5", data: { exit: 0, ms: 5 } }} />);
  expect(html).toContain("Edited</span>");
  expect(html).toContain("2 files");
  expect(html).toContain("+3</span>");
  expect((html.match(/class="edit-file"/g) ?? []).length).toBe(2);
  expect(html).not.toContain("patched src/math.js");
  expect(html).toContain("5");
});

test("a turn's files chip opens Changes on that turn", async () => {
  const { TurnFiles } = await import("../src/app");
  const { WorkContext } = await import("../src/work-ui");
  const turn = groupTurns([
    { seq: 7, at: at(0), kind: "input", text: "go" },
    { seq: 8, at: at(1), kind: "code", text: 'tools.write("a.js", "1\\n2\\n")' },
    { seq: 9, at: at(2), kind: "done", text: "", data: { exit: 0, files: ["a.js"] } },
  ])[0];
  const html = renderToStaticMarkup(<WorkContext.Provider value={{ session: "s1" } as never}><TurnFiles files={["a.js"]} turn={turn} /></WorkContext.Provider>);
  expect(html).toContain('href="#/s/s1/changes?turn=7"');
  expect(html).toContain("+2</span>");
});

// R3-C: a steer the server took while the turn ran is part of that turn.
test("R3-C: a steer recorded while the turn ran stays inside that turn", () => {
  const lines = [
    { seq: 1, at: at(0), kind: "input", text: "write a story" },
    { seq: 2, at: at(3), kind: "input", text: "say hi", data: { steer: true } },
    { seq: 3, at: at(4), kind: "assistant", text: "Hi there" },
    { seq: 4, at: at(5), kind: "done", text: "" },
  ] as Line[];
  const turns = groupTurns(lines);
  expect(turns.length).toBe(1);
  const html = renderToStaticMarkup(<TurnView turn={turns[0]} />);
  expect(html).toContain("steer-note");
  expect(html).toContain("say hi");
  expect(html).not.toContain("Interrupted");
});

test("R3-C: a cut-off turn uses the same stop word and duration as a stopped one", () => {
  const html = renderToStaticMarkup(<TurnView turn={groupTurns([live[0], live[1]])[0]} superseded />);
  expect(html).toContain("Stopped");
  expect(html).toContain("Worked for 1s");
  expect(html).not.toContain("Turn: ");
});

// R3-F: honest failed rows, matching counts, keyboard reach.
const failCode = 'console.log(tools.write("b.txt", "a\\nb\\nc"))\nconsole.log(tools.bash("printf x >> a.txt"))';
const failResult: Line = { seq: 2, at: at(1), kind: "result", text: "wrote b.txt\nerror: GoError: bash: printf x: exit status 2 \u2014 /var/folders/55/mh/T/bough-bash-2729125139.sh: line 1: printf: --: invalid option",
  data: { code: failCode, exit: 2, ms: 15, error: "GoError: bash: printf x: exit status 2 \u2014 /var/folders/55/mh/T/bough-bash-2729125139.sh: line 1: printf: --: invalid option" } };

test("a failed edit says Edit failed, with no success verb or +N", () => {
  const html = renderToStaticMarkup(<ToolCall code={{ seq: 1, at: at(0), kind: "code", text: failCode }} result={failResult} />);
  expect(html).toContain("Edit failed</span>");
  expect(html).not.toContain("Edited</span>");
  expect(/<summary[\s\S]*?<\/summary>/.exec(html)![0]).not.toContain("rt-add");
  expect(html).not.toContain("GoError");
  expect(html).not.toContain("/var/folders");
  expect(html).toContain("printf: --: invalid option");
});

test("a group counts no failed edit, times under a second as <1s, and keeps its buttons out of summary", () => {
  const ls: Line[] = [
    { seq: 1, at: at(0), kind: "code", text: failCode }, failResult,
    { seq: 3, at: at(2), kind: "code", text: 'console.log(tools.bash("ls"))' },
    { seq: 4, at: at(3), kind: "result", text: "a", data: { exit: 0, ms: 5 } },
    { seq: 5, at: at(4), kind: "code", text: 'console.log(tools.bash("pwd"))' },
    { seq: 6, at: at(5), kind: "result", text: "/", data: { exit: 0, ms: 5 } },
  ];
  const html = renderToStaticMarkup(<ToolRun lines={ls} codes={[]} />);
  const summary = /<summary[\s\S]*?<\/summary>/.exec(html)![0];
  expect(summary).not.toContain("Edited b.txt");
  expect(summary).not.toContain("rt-add");
  expect(summary).toContain("&lt;1s");
  expect(summary).not.toContain(" 0s");
  expect(summary).not.toContain("<button");
  expect(html).toContain("1 failed");
});

test("a group's and a turn's edit counts come from the checkpoint diff when given", async () => {
  const { TurnFiles } = await import("../src/app");
  const { WorkContext } = await import("../src/work-ui");
  const diff = [{ path: "b.txt", add: 3, del: 0 }, { path: "a.txt", add: 3, del: 0 }];
  const html = renderToStaticMarkup(<ToolRun lines={edits} codes={[]} turnEdits={diff} />);
  expect(html).toContain("+6</span>");
  const turn = groupTurns([
    { seq: 7, at: at(0), kind: "input", text: "go" },
    { seq: 8, at: at(1), kind: "code", text: 'tools.write("b.txt", "1\\n2\\n3\\n")' },
    { seq: 9, at: at(2), kind: "done", text: "", data: { exit: 0, files: ["a.txt", "b.txt"] } },
  ])[0];
  const foot = renderToStaticMarkup(<WorkContext.Provider value={{ session: "s1" } as never}><TurnFiles files={["a.txt", "b.txt"]} turn={turn} edits={diff} /></WorkContext.Provider>);
  expect(foot).toContain("+6</span>");
  expect(foot).toContain("<button");
  expect(foot).not.toContain("<a ");
});

test("a turn that ended on an error states Failed once, in the error", () => {
  const ls: Line[] = [...live.slice(0, 1),
    { seq: 2, at: at(1), kind: "error", text: "401 Unauthorized" },
    { seq: 3, at: at(2), kind: "done", text: "", data: {} }];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(ls)[0]} />);
  expect(html).not.toContain(">Failed<");
  expect(html).not.toContain(">Done<");
});
