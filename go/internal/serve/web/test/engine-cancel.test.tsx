import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
const { ToolCall } = await import("../src/app");
const { groupTools, splitWork } = await import("../src/render");
import type { Line } from "../src/types";

// A run_js the person stopped, as the engine records it
// (internal/unreal/project): a result with the cancel as its error, and
// canceled set, as a stopped native call has.
const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const js = "tools.bash('sleep 100')";
const code: Line = { seq: 2, at: at(0), kind: "code", text: js };
const stopped: Line = { seq: 3, at: at(2), kind: "result", text: "Cancelled: cancelled by the user", data: { code: js, error: "cancelled by the user", canceled: true, ms: 2000 } };
const ask: Line = { seq: 4, at: at(2), kind: "call", text: "Which colour?", data: { id: "toolu_4", tool: "ask", error: "cancelled by the user", canceled: true } };

test("EC: a stopped program reads Cancelled, not failed", () => {
  const html = renderToStaticMarkup(<ToolCall code={code} result={stopped} />);
  expect(html).toContain("tool-stopped");
  expect(html).toContain("Cancelled");
  expect(html).not.toContain("block-failed");
  expect(html).not.toContain("fail-lead");
});

test("EC: a stopped program or call is not counted as a failure", () => {
  const items = groupTools([code, stopped, ask].map((l) => ({ kind: "line" as const, seq: l.seq, line: l })), [js]);
  const [seg] = splitWork(items, [js], false);
  if (seg.kind !== "work") throw new Error("want a work segment");
  expect(seg.failed).toBe(0);
});
