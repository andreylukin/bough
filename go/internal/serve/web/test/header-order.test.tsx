import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { ChangesChip, SessionChanges, Thread } from "../src/app";
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

// R4-F: an empty session reads "No edits", not "Session edits None".
test("R4-F: a session with no edits says No edits", () => {
  const none = { files: [], repo: true, failed: false };
  const html = renderToStaticMarkup(
    <SessionChanges.Provider value={{ session: none, tree: none, turn: undefined, retry: () => {} } as never}>
      <ChangesChip row={row} />
    </SessionChanges.Provider>);
  const chip = html.slice(0, html.indexOf("</summary>"));
  expect(chip).toContain(">No edits<");
  expect(chip).not.toContain(">None<");
  expect(chip).not.toContain(">Session edits<");
});

// MB-HDR: the header is one inline-property style block; secondary actions share one height and ⚙ is a ghost.
test("MB-HDR: header CSS block sets one action height, a ghost settings button and a borderless overflow popover", () => {
  const css = require("node:fs").readFileSync(new URL("../dist/index.html", import.meta.url), "utf8") as string;
  const block = css.slice(css.indexOf("/* MB-HDR"), css.indexOf("/* /MB-HDR */"));
  expect(block.length).toBeGreaterThan(0);
  expect(block).toMatch(/\.work-summary,\.thread-head \.head-ack\{height:32px;min-height:32px/);
  expect(block).toMatch(/\.thread-head \.more\{[^}]*border:0/);
  expect(block).toMatch(/\.rt-more>\.rt-pop,\.rt-pop\.rt-diff\{border:0/);
});

test("MB-HDR: Edits is the short visible label, the aria label keeps Session edits", () => {
  const edits = { files: [{ path: "a", add: 1, del: 0 }], repo: true, failed: false };
  const html = renderToStaticMarkup(
    <SessionChanges.Provider value={{ session: edits, tree: edits, turn: undefined, retry: () => {} } as never}>
      <ChangesChip row={row} />
    </SessionChanges.Provider>);
  expect(html).toContain(">Edits<");
  expect(html).toContain("Session edits:");
});
