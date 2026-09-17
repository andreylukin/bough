import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Thread } from "../src/app";
import type { Line, Row } from "../src/types";

const row = { id: "s1", cwd: "/tmp/x", status: "done", title: "Header", usage: { in: 10, out: 5, lastIn: 10, cost: 0.03 } } as unknown as Row;
const lines = [
  { seq: 1, kind: "input", text: "hi", at: "2026-09-16T00:00:00Z" },
  { seq: 2, kind: "done", text: "", at: "2026-09-16T00:00:02Z" },
] as unknown as Line[];
const props = {
  onSend: async () => null, onAnswer: async () => null, onInterrupt: () => {}, onArchive: () => {}, onRename: async () => {},
  onModel: () => {}, onEffort: () => {}, onAssign: () => {}, onBack: () => {}, onContext: () => {}, onAck: () => {},
  projects: [], busy: false, jump: null,
};

test("the header's Tab order is its visual order: the strip's chips, then Settings", () => {
  const html = renderToStaticMarkup(<Thread row={row} lines={lines} {...props} />);
  const strip = html.indexOf('class="runtime-strip"'), settings = html.indexOf('aria-label="Session settings"');
  expect(strip).toBeGreaterThan(-1);
  expect(settings).toBeGreaterThan(strip);
});

// R3-G: two groups. Identity (title, repo, status) on the left; the metrics
// cluster, then the actions (Work, Mark seen) on the right, never inside the title row.
test("R3-G: Mark seen sits with the actions after the metrics, not in the title group", () => {
  const html = renderToStaticMarkup(<Thread row={{ ...row, trouble: "tests failed", repo: "github.com/a/head-repo" } as Row} lines={lines} {...props} />);
  const main = html.slice(html.indexOf('class="head-main"'), html.indexOf('class="runtime-strip"'));
  expect(main).toContain("<h1");
  expect(main).toContain("head-repo");
  expect(main).not.toContain("Mark seen");
  const metrics = html.indexOf('class="rt-metrics"'), actions = html.indexOf('class="rt-actions"');
  const ack = html.indexOf(">Mark seen<"), settings = html.indexOf('aria-label="Session settings"');
  expect(metrics).toBeGreaterThan(-1);
  expect(html.indexOf("Context", metrics)).toBeLessThan(actions);
  expect(actions).toBeGreaterThan(metrics);
  expect(ack).toBeGreaterThan(actions);
  expect(settings).toBeGreaterThan(ack);
});
