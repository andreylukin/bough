import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { ToolCall, ToolRun, RunningCallCtx } = await import("../src/app");
const { callsHeadline, splitWork, groupTools } = await import("../src/render");
import type { Line } from "../src/types";

const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const block = 'tools.view("a.go"); tools.patch("a.go", "x", "y"); tools.bash("go test ./...")';
const code: Line = { seq: 1, at: at(0), kind: "code", text: block };
const calls: Line[] = [
  { seq: 2, at: at(1), kind: "call", text: "a.go", data: { tool: "view", id: 1, ms: 3 } },
  { seq: 3, at: at(2), kind: "call", text: "a.go", data: { tool: "patch", id: 2, ms: 5, add: 2, del: 1 } },
  { seq: 4, at: at(9), kind: "call", text: "go test ./...", data: { tool: "bash", id: 3, ms: 7000, exit: 1, error: "go test ./...: exit status 1 — FAIL\tpkg" } },
];
const result: Line = { seq: 5, at: at(9), kind: "result", text: "error: bash: go test ./...: exit status 1", data: { code: block, exit: 1, ms: 7100 } };

test("CR: a block with recorded calls is named by them, not by the regex over its source", () => {
  expect(callsHeadline(calls)).toBe("Ran 1 command, read 1 file, edited 1 file");
  const html = renderToStaticMarkup(<ToolCall code={code} result={result} calls={calls} />);
  expect(html).toContain('<span class="block-label">Ran 1 command, read 1 file, edited 1 file</span>');
  // One call: that call's verb and detail.
  const one = renderToStaticMarkup(<ToolCall code={code} result={result} calls={[calls[2]]} />);
  expect(one).toContain('<span class="block-label">Ran</span>');
  expect(one).toContain('title="go test ./..."');
});

test("CR: each call is a row with its mark, evidence, and the failed one's reason", () => {
  const html = renderToStaticMarkup(<ToolCall code={code} result={result} calls={calls} />);
  const rows = html.match(/<li class="call-row[^"]*">/g) ?? [];
  expect(rows.length).toBe(3);
  expect(html).toContain('<span class="call-verb">Read</span>');
  expect(html).toContain('<span class="call-verb">Patched</span>');
  expect(html).toContain('<span class="rt-add">+2</span><span class="rt-del">−1</span>');
  expect(html).toContain('<li class="call-row call-row-failed">');
  expect(html).toContain('<span class="tool-meta-failed">exit 1</span>');
  expect(html).toContain('class="call-why"');
  expect(html).toContain("exit status 1 — FAIL");
});

test("CR: while the block runs, the call in flight is the row's headline and a spinning last row, and the block is open", () => {
  const running = { kind: "call", tool: "bash", detail: "go test ./...", at: at(3) };
  const html = renderToStaticMarkup(
    <RunningCallCtx.Provider value={running}><ToolCall code={code} calls={calls.slice(0, 2)} live /></RunningCallCtx.Provider>,
  );
  expect(html).toContain('<span class="block-label">Running</span>');
  expect(html).toContain('<li class="call-row call-row-running">');
  expect(html).toContain("spin-mark");
  expect(html).toMatch(/<details class="block thin toolcall"[^>]* open/);
  // A finished block does not stay open on its own.
  const done = renderToStaticMarkup(<ToolCall code={code} result={result} calls={calls} />);
  expect(done).not.toMatch(/<details class="block thin toolcall"[^>]* open/);
  // A child's call is not the parent block's: no running row from it.
  const child = renderToStaticMarkup(
    <RunningCallCtx.Provider value={{ ...running, kind: "sub:call" }}><ToolCall code={code} calls={[]} live /></RunningCallCtx.Provider>,
  );
  expect(child).not.toContain("call-row-running");
});

test("CR: a run hands each block the calls recorded between it and its result", () => {
  const second: Line = { seq: 6, at: at(10), kind: "code", text: 'tools.bash("ls")' };
  const c2: Line = { seq: 7, at: at(11), kind: "call", text: "ls", data: { tool: "bash", id: 4, ms: 2, exit: 0 } };
  const r2: Line = { seq: 8, at: at(11), kind: "result", text: "a.go", data: { code: 'tools.bash("ls")', exit: 0, ms: 3 } };
  const html = renderToStaticMarkup(<ToolRun lines={[code, ...calls, result, second, c2, r2]} codes={[block, 'tools.bash("ls")']} />);
  // Three rows for the three-call block; the one-call block is its own summary line and lists no rows.
  expect((html.match(/<li class="call-row[^"]*">/g) ?? []).length).toBe(3);
  expect(html).toContain('title="ls"');
  // A call row is never a loose entry of the run.
  expect(html).not.toContain("call-rows-nested");
});

test("CR: the segment's live step is the call in flight, and call rows do not count as rows or actions", () => {
  const lines: Line[] = [code, calls[0], calls[1]];
  const items = groupTools(lines.map((l) => ({ kind: "line" as const, seq: l.seq, line: l })), [block]);
  const segs = splitWork(items, [block], true);
  const work = segs.find((s) => s.kind === "work")!;
  expect(work.actions).toBe(1);
  expect(work.step).toBe("Patching a.go");
});
