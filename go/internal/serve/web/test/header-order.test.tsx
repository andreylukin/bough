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
