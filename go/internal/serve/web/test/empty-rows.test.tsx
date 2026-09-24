import { afterAll, beforeAll, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Sidebar, noSessionsListed } from "../src/app";
import { recentSessions } from "../src/palette";
import type { Row } from "../src/types";

// An empty session (started with no prompt) whose child is gone is left
// out of the sidebar and the palette's Recent: go/tests/model/specs/
// empty_session_lifecycle.fizz. Two of its claims failed on the page:
// a renamed one disappeared too (NamedNeverHidden), and the welcome's
// gate counted the hidden ones, so after a serve restart the sidebar
// said "No sessions yet." with no welcome (NoSessionsMeansWelcome).
const g = globalThis as unknown as { window?: object };
beforeAll(() => { g.window ??= {}; });
afterAll(() => { delete g.window; });

const at = new Date(Date.now() - 60_000).toISOString();
const row = (id: string, over: Partial<Row> = {}): Row => ({
  id, title: "", cwd: "/w/work", status: "idle", live: false, archived: false, entries: 0,
  modified: at, lastAt: at, mode: "local", empty: true, ...over,
} as Row);

const listed = (rows: Row[]) => [...renderToStaticMarkup(<Sidebar rows={rows} projects={[]} selected={null}
  onSelect={() => {}} query="" onQuery={() => {}} showArchived={false} onToggleArchived={() => {}} />)
  .matchAll(/data-id="([^"]+)"/g)].map((m) => m[1]);

test("a named empty session stays listed once its child is gone", () => {
  const rows = [row("named", { title: "named 1" }), row("blank")];
  expect(listed(rows)).toEqual(["named"]);
  expect(recentSessions(rows, null, [], 8).map((r) => r.id)).toEqual(["named"]);
});

test("the welcome's gate counts only what the sidebar lists", () => {
  expect(noSessionsListed([row("blank")])).toBe(true);
  expect(noSessionsListed([row("blank", { archived: true })])).toBe(true);
  expect(noSessionsListed([row("up", { live: true })])).toBe(false);
  expect(noSessionsListed([row("named", { title: "named 1" })])).toBe(false);
  expect(noSessionsListed([row("sent", { empty: false })])).toBe(false);
  // A project's thread lists in its project's group however empty.
  expect(noSessionsListed([row("thread", { mode: "project", project: "bough" })])).toBe(false);
});
