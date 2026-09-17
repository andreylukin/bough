import { expect, mock, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import type { Command } from "../src/palette";
// No DOM under bun: the portal renders in place.
const real = await import("react-dom");
mock.module("react-dom", () => ({ ...real, createPortal: (node: unknown) => node }));
(globalThis as { document?: unknown }).document ??= { body: {} };
const { Palette } = await import("../src/palette");

test("'>' lists every command with each group heading once", () => {
  const c = (id: string, group: string): Command => ({ id, label: id, group, run: () => {} });
  const commands = [c("new-a", "Start"), c("welcome", "Navigation"), c("project", "Start"), c("sessions", "Navigation"), c("ingest", "Wiki"), c("archived", "Navigation")];
  const html = renderToStaticMarkup(<Palette open onClose={() => {}} rows={[]} commands={commands} onOpenSession={() => {}} initialQuery=">" />);
  const heads = [...html.matchAll(/class="pal-group[^"]*"[^>]*>([^<]*)</g)].map((m) => m[1]);
  expect(heads).toEqual(["Start", "Navigation", "Wiki"]);
});

const row = (id: string, extra: object = {}) => ({ id, title: "fix the " + id, lastAt: new Date().toISOString(), status: "error", repo: "work", ...extra }) as never;
const render = (q: string, extra: object = {}) => renderToStaticMarkup(<Palette open onClose={() => {}} rows={[row("aaa111"), row("bbb222", { status: "done" })]} commands={[]} onOpenSession={() => {}} initialQuery={q} {...extra} />);
const headsOf = (html: string) => [...html.matchAll(/class="pal-group[^"]*"[^>]*>([^<]*)</g)].map((m) => m[1]);

test("a query that is only filters lists its sessions under Sessions, never Mentioned in", () => {
  const html = render("status:failed");
  expect(headsOf(html)).toEqual(["Sessions"]);
  expect(html).toContain('class="pal-ops"');
});

test("when only Start matches, the palette says nothing else did and how to start", () => {
  const html = render("zzqqxx", { onStart: () => {} });
  expect(html).toContain("No results for “zzqqxx”");
  expect(html).toContain("to start a session with it.");
});

test("the filter tip lives in the list on an empty query only; the old strip is gone", () => {
  expect(render("")).toContain('class="pal-tip"');
  expect(render("")).not.toContain("pal-syntax");
  expect(render("fix")).not.toContain('class="pal-tip"');
});

test("the footer counts results in words and never shows a count beside Searching", () => {
  const html = render("status:failed");
  expect(html).toContain("1 result<");
  expect(html).not.toContain("shown");
});
