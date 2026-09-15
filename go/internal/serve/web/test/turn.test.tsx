import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
// No DOM under bun: markdown sanitising is not what these assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { Entry, TurnView } = await import("../src/app");
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
