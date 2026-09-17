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
