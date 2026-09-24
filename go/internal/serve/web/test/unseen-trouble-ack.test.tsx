import { afterAll, beforeAll, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Sidebar } from "../src/app";
import type { Row } from "../src/types";

// Sidebar reads window.navigation at mount; a bare object is enough for a static render.
const g = globalThis as unknown as { window?: object };
beforeAll(() => { g.window ??= {}; });
afterAll(() => { delete g.window; });

const at = (h: number) => new Date(Date.now() - h * 3_600_000).toISOString();
const row = (id: string, over: Partial<Row> = {}): Row => ({
  id, title: `session ${id}`, cwd: "/w/bough", repo: "/w/bough", status: "idle", live: false, archived: false, entries: 3,
  modified: at(1), lastAt: at(1), mode: "local", ...over,
});

const render = (rows: Row[]) => renderToStaticMarkup(
  <Sidebar rows={rows} selected={null} onSelect={() => {}} query="" onQuery={() => {}} showArchived={false} onToggleArchived={() => {}} onAck={() => {}} />);

// A troubled local session is lifted out of its folder into Needs you,
// so the pinned copy is the only row it has: without Seen there, the
// sidebar offered no way to mark it seen (unseen_trouble_ack MarkSeen).
test("a troubled session lifted to Needs you keeps its Seen", () => {
  const html = render([row("bad", { status: "error", trouble: "failed", turns: 1 }), row("ok", { status: "done", turns: 1 })]);
  expect(html).toContain('aria-label="Mark session bad seen"');
});

// A project's thread is pinned and also stays in its project group,
// which carries the Seen; the pin does not repeat it.
test("a project thread's pin does not repeat the Seen its group row has", () => {
  const html = render([row("pt", { status: "error", trouble: "failed", turns: 1, mode: "project", project: "web" })]);
  expect(html.match(/aria-label="Mark session pt seen"/g)?.length).toBe(1);
});
