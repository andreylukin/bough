import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
const { TurnView, withUnendedAsks } = await import("../src/app");
const { groupTurns } = await import("../src/render");
import type { Line } from "../src/types";

const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();
const input: Line = { seq: 1, at: at(0), kind: "input", text: "Set it up." };
const ask = (seq: number, id: string, secret = false): Line =>
  ({ seq, at: at(seq), kind: "ask", text: "", data: { id, question: secret ? "the API token" : "Which colour?", options: [], ...(secret ? { secret: true } : {}) } });
const ended = (seq: number, tool: string): Line =>
  ({ seq, at: at(seq), kind: "call", text: "Which colour?", data: { id: "toolu_" + seq, tool, output: "red" } });

test("UA: an ask whose child died before it returned stays in the transcript, stopped", () => {
  const lines = [input, ask(2, "ask-1"), ended(3, "ask"), ask(4, "ask-2", true)];
  const shown = withUnendedAsks(lines, "interrupted", "");
  expect(shown.length).toBe(5);
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(shown)[0]} />);
  expect(html).toContain('<span class="block-label">Secret</span>');
  expect(html).toContain("the API token");
  expect(html).toContain("Interrupted</span>");
  expect(html).not.toContain("spin-mark");
});

test("UA: an ask that ended, is open, or may still end adds nothing", () => {
  const done = [input, ask(2, "ask-1"), ended(3, "ask")];
  expect(withUnendedAsks(done, "interrupted", "")).toBe(done);
  // On screen: its own running row and card show it.
  const open = [input, ask(2, "ask-1")];
  expect(withUnendedAsks(open, "needs-you", "ask-1")).toBe(open);
  // Timed out a moment ago: its call's end is on its way.
  expect(withUnendedAsks(open, "running", "")).toBe(open);
  // A code block's ask: the block itself shows where the turn stopped.
  const block = [input, { seq: 2, at: at(2), kind: "code", text: "await tools.ask('Which colour?')" }, ask(3, "ask-1")];
  expect(withUnendedAsks(block, "interrupted", "")).toBe(block);
});

test("UA: a turn that moved on leaves its unended ask stopped, whatever the session does now", () => {
  const lines = [input, ask(2, "ask-1"), { seq: 3, at: at(3), kind: "input", text: "hello" }];
  const shown = withUnendedAsks(lines, "running", "");
  expect(shown.map((l) => l.kind)).toEqual(["input", "ask", "call", "input"]);
});
