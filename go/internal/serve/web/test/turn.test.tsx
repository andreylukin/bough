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
