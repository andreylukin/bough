import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { ToolCall, TurnView } = await import("../src/app");
const { groupTurns, workHeadline } = await import("../src/render");
const { EditDiff } = await import("../src/changes");
import type { Line } from "../src/types";

const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const bash = 'console.log(tools.bash("go test ./..."))';

test("MB-TR: a successful call does not say exit 0; a failed one says exit 1 in red with the glyph", () => {
  const ok = renderToStaticMarkup(<ToolCall code={{ seq: 1, at: at(0), kind: "code", text: bash }} result={{ seq: 2, at: at(20), kind: "result", text: "ok", data: { code: bash, exit: 0, ms: 20000 } }} />);
  expect(ok).not.toContain("exit 0");
  expect(ok).toContain("20s");
  const bad = renderToStaticMarkup(<ToolCall code={{ seq: 1, at: at(0), kind: "code", text: bash }} result={{ seq: 2, at: at(1), kind: "result", text: "FAIL\tpkg\nexit status 1", data: { code: bash, exit: 1, ms: 10 } }} current />);
  expect(bad).toContain('<span class="tool-meta-failed">exit 1</span>');
  expect(bad).toContain("fail-mark");
  expect(bad).toContain('<span class="fail-lead">Failed</span>');
  expect(bad).not.toContain("tool-state-failed");
});

test("MB-TR: a stopped call is neutral: Interrupted with a square, and one line for the missing result", () => {
  const html = renderToStaticMarkup(<ToolCall code={{ seq: 1, at: at(0), kind: "code", text: bash }} stopped />);
  expect(html).toContain("tool-stopped");
  expect(html).toContain("stop-mark");
  expect(html).toContain("No result was recorded.");
  expect(html).not.toContain("result not recorded");
});

test("MB-TR: a fold never reads bare Worked", () => {
  expect(workHeadline({ actions: 2, thinkingOnly: false, from: at(0), to: at(23) })).toBe("Worked for 23s · 2 actions");
  expect(workHeadline({ actions: 2, thinkingOnly: false, from: at(0), to: at(0) })).toBe("2 actions");
  expect(workHeadline({ actions: 0, thinkingOnly: false, from: "", to: "" })).not.toBe("Worked");
});

test("MB-TR: an inline edit diff has a sign gutter and an Unchanged lines strip", () => {
  const html = renderToStaticMarkup(<EditDiff part={{ kind: "edit", path: "src/a/main.go", head: "", lines: ["-old", "…", "+new"], add: 1, del: 1 }} />);
  expect(html).toContain('<span class="edit-diff-dir">src/a/</span>main.go');
  expect(html).toContain('<span class="dl-s" aria-hidden="true">+</span><span class="dl-t">new');
  expect(html).toContain("Unchanged lines");
});

test("MB-TR: a steer note is word then text, with no mark column", () => {
  const lines = [
    { seq: 1, at: at(0), kind: "input", text: "hi" },
    { seq: 2, at: at(3), kind: "input", text: "say hi", data: { steer: true } },
    { seq: 3, at: at(4), kind: "assistant", text: "hi" },
    { seq: 4, at: at(5), kind: "done", text: "" },
  ] as Line[];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).toContain('<p class="steer-note"><span class="steer-word">Steer</span>');
});

test("MB-TR: a stopped turn's foot is neutral with the square", () => {
  const lines = [
    { seq: 1, at: at(0), kind: "input", text: "go" },
    { seq: 2, at: at(5), kind: "cancelled", text: "" },
    { seq: 3, at: at(11), kind: "done", text: "" },
  ] as Line[];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).toMatch(/turn-outcome turn-stopped"><span class="stop-mark"[^>]*><\/span>Stopped/);
});
