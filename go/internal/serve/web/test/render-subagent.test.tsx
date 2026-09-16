import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
// No DOM under bun: markdown sanitising is not what these assert.
mock.module("dompurify", () => ({ default: { sanitize: (s: string) => s } }));
const { TurnView } = await import("../src/app");
const { groupTurns } = await import("../src/render");
import type { Line } from "../src/types";

const t0 = Date.parse("2026-01-02T10:00:00Z");
const at = (s: number) => new Date(t0 + s * 1000).toISOString();

test("a program whose spawn finished with a result shows the card's state, not 'Result not recorded'", () => {
  const lines: Line[] = [
    { seq: 1, at: at(0), kind: "input", text: "Check the watchers." },
    { seq: 2, at: at(1), kind: "code", text: 'await tools.spawn("Check the watchers")' },
    { seq: 3, at: at(2), kind: "sub:start", text: "Check the watchers", data: { worker: 1 } },
    { seq: 4, at: at(9), kind: "sub:done", text: "All watchers use the clock.", data: { worker: 1, status: "ok", steps: 2 } },
    { seq: 5, at: at(10), kind: "done", text: "" },
  ];
  const html = renderToStaticMarkup(<TurnView turn={groupTurns(lines)[0]} />);
  expect(html).not.toContain("Result not recorded");
  expect(html).toContain("Finished");
});
