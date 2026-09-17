import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
// No DOM under bun: markdown sanitising is not what these assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { TurnView } = await import("../src/app");
const { groupSubs, groupTools, groupTurns, isReply, splitWork, workHeadline } = await import("../src/render");
import type { Line } from "../src/types";

const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const bash = (c: string) => `tools.bash("${c}")`;

const lines: Line[] = [
  { seq: 1, at: at(0), kind: "input", text: "Make the tests fast." },
  { seq: 2, at: at(1), kind: "thinking", text: "The suite sleeps." },
  { seq: 3, at: at(4), kind: "code", text: bash("go test ./...") },
  { seq: 4, at: at(9), kind: "result", text: "FAIL", data: { code: bash("go test ./..."), exit: 1 } },
  { seq: 5, at: at(10), kind: "todo/add", text: "fake clock", data: { id: 1 } },
  { seq: 6, at: at(13), kind: "assistant", text: "The watcher sleeps; I'll inject a clock." },
  { seq: 7, at: at(14), kind: "assistant", text: "```js\n" + bash("sed -i s/a/b/ w.go") + "\n```" },
  { seq: 8, at: at(14), kind: "code", text: bash("sed -i s/a/b/ w.go") },
  { seq: 9, at: at(15), kind: "result", text: "", data: { code: bash("sed -i s/a/b/ w.go"), exit: 0 } },
  { seq: 10, at: at(16), kind: "job", text: "job 4 [exited 2] go vet ./... (1s)" },
  { seq: 11, at: at(26), kind: "todo/done", text: "", data: { id: 1 } },
  { seq: 12, at: at(27), kind: "assistant", text: "Done: the suite runs in 0.4s." },
  { seq: 13, at: at(28), kind: "done", text: "", data: { exit: 0 } },
];
const segsOf = (ls: Line[], live = false) => {
  const turn = groupTurns(ls)[0];
  const codes = turn.body.filter((l) => l.kind === "code").map((l) => l.text);
  return splitWork(groupTools(groupSubs(turn.body), codes), codes, live);
};

test("a reply renders words; a program-only reply or a skipped-blocks note is not one", () => {
  expect(isReply(lines[5], [])).toBe(true);
  expect(isReply(lines[6], [lines[7].text])).toBe(false);
  expect(isReply({ ...lines[6], text: "[2 further code block(s) dropped — a reply runs at most 1 blocks]" }, [])).toBe(false);
  expect(isReply({ ...lines[5], kind: "thinking" }, [])).toBe(false);
});

test("work between replies splits into segments with actions, failures and duration", () => {
  const segs = segsOf(lines);
  expect(segs.map((s) => s.kind)).toEqual(["work", "reply", "work", "reply"]);
  const [a, , b] = segs as Extract<(typeof segs)[number], { kind: "work" }>[];
  expect(a.actions).toBe(2); // one call + one todo change
  expect(a.failed).toBe(1);
  expect(workHeadline(a)).toBe("Worked for 9s · 2 actions");
  expect(b.actions).toBe(3); // call + job + todo
  expect(b.failed).toBe(1); // the job exited 2
  expect(b.last).toBe(false);
  expect(workHeadline(b)).toBe("Worked for 12s · 3 actions");
});

test("thinking alone reads Thought for, until the reply it led to; one action is singular", () => {
  const segs = segsOf([lines[0], lines[1], { ...lines[5], at: at(8) }, lines[12]]);
  expect(workHeadline(segs[0] as never)).toBe("Thought for 7s");
  expect(workHeadline({ actions: 1, thinkingOnly: false, from: at(0), to: at(0.2) })).toBe("1 action");
});

test("a live turn's last segment carries the current step; running subagents stay out of it", () => {
  const live: Line[] = [...lines.slice(0, 6), { seq: 20, at: at(30), kind: "code", text: bash("go test -run Clock ./kernel") },
    { seq: 21, at: at(31), kind: "sub:start", text: "Check the other watchers", data: { worker: 1 } }];
  const segs = segsOf(live, true);
  expect(segs.map((s) => s.kind)).toEqual(["work", "reply", "work", "pinned"]);
  const w = segs[2] as Extract<(typeof segs)[number], { kind: "work" }>;
  expect(w.last).toBe(true);
  expect(w.step).toBe("Running go test -run Clock ./kernel");
});

test("prose sent with its own program follows that program's work, which stays the running segment", () => {
  const prog = bash("sleep 40");
  const live: Line[] = [lines[0], { seq: 2, at: at(1), kind: "assistant", text: "Sleeping now; done.\n\n```js\n" + prog + "\n```" },
    { seq: 3, at: at(1), kind: "code", text: prog }];
  const segs = segsOf(live, true);
  expect(segs.map((s) => s.kind)).toEqual(["work", "reply"]);
  expect((segs[0] as Extract<(typeof segs)[number], { kind: "work" }>).last).toBe(true);
  expect((segs[1] as Extract<(typeof segs)[number], { kind: "reply" }>).early).toBe(true);
  // Words without a program keep their place.
  expect(segsOf(lines).map((s) => s.kind)).toEqual(["work", "reply", "work", "reply"]);
});

test("prose held for its program also follows a subagent that program started and is still running", () => {
  const prog = bash("sleep 40");
  const live: Line[] = [lines[0], { seq: 2, at: at(1), kind: "assistant", text: "Spawning a helper.\n\n```js\n" + prog + "\n```" },
    { seq: 3, at: at(1), kind: "code", text: prog },
    { seq: 4, at: at(2), kind: "sub:start", text: "Check the other watchers", data: { worker: 1 } }];
  expect(segsOf(live, true).map((s) => s.kind)).toEqual(["work", "pinned", "reply"]);
});

test("a finished long turn shows collapsed work rows and the replies", () => {
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).toContain("Worked for 9s · 2 actions");
  expect(html).toContain("The watcher sleeps; I&#39;ll inject a clock.");
  expect(html).toContain("Done: the suite runs in 0.4s.");
  expect(html.match(/class="block thin work-seg"/g)?.length).toBe(2);
  expect(html).not.toMatch(/class="block thin work-seg"[^>]*open=""/);
  expect(html).toContain("1 failed");
  expect(html).toContain("Expand all");
});

test("a turn that ended on a failed command opens its last segment", () => {
  const failed = [...lines.slice(0, 6), { seq: 20, at: at(20), kind: "code", text: bash("go test ./...") },
    { seq: 21, at: at(22), kind: "result", text: "FAIL", data: { code: bash("go test ./..."), exit: 1 } },
    { seq: 22, at: at(23), kind: "error", text: "go: exit status 1" },
    { seq: 23, at: at(24), kind: "done", text: "", data: { exit: 1 } }];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(failed)[0]} />);
  const rows = [...html.matchAll(/<details class="block thin work-seg"[^>]*>/g)].map((m) => m[0]);
  expect(rows.length).toBe(2);
  expect(rows[0]).not.toContain(`open=""`);
  expect(rows[1]).toContain('open=""');
});

test("a running turn's work row says Working and its current step, once", () => {
  const live = lines.slice(0, 5);
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(live)[0]} working="Working" />);
  expect(html).toContain("work-seg-live");
  // R4-D: its result landed, so the step reads done.
  expect(html).toContain("Ran go test ./...");
  expect(html).not.toContain('class="working"');
});

test("a turn that ends on an error after its work shows the error card outside the folded work, not as a failed action", () => {
  const view = bash("cat a.go");
  const read = bash("cat b.go");
  const ended: Line[] = [lines[0],
    { seq: 2, at: at(1), kind: "assistant", text: "```js\n" + view + "\n" + read + "\n```" },
    { seq: 3, at: at(1), kind: "code", text: view },
    { seq: 4, at: at(2), kind: "result", text: "a", data: { code: view, exit: 0 } },
    { seq: 5, at: at(2), kind: "code", text: read },
    { seq: 6, at: at(3), kind: "result", text: "b", data: { code: read, exit: 0 } },
    { seq: 7, at: at(4), kind: "error", text: "the model returned an empty reply twice (provider hiccup)" },
    { seq: 8, at: at(4), kind: "done", text: "", data: {} }];
  const segs = segsOf(ended);
  expect(segs.map((s) => s.kind)).toEqual(["work", "notice"]);
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(ended)[0]} />);
  expect(html).not.toContain("1 failed");
  const seg = html.indexOf('class="work-seg-body"'), card = html.indexOf("err-card");
  expect(card).toBeGreaterThan(-1);
  expect(seg === -1 || html.slice(seg, card).includes("</details>")).toBe(true);
});
