import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { HistoryArrows, focusAppOnRoute } from "../src/app";

test("a route change focuses .app, so the next Tab reaches the skip links", () => {
  const app = { tabIndex: -1, focus() { doc.activeElement = app; } };
  const doc = { activeElement: {} as unknown, querySelector: (s: string) => (s === ".app" ? app : null) };
  focusAppOnRoute(doc as unknown as Document);
  expect(doc.activeElement).toBe(app);
});

test("a route change leaves focus alone inside a dialog or a field", () => {
  const app = { focus() { throw new Error("stole focus"); } };
  const input = { tagName: "INPUT", closest: () => null };
  focusAppOnRoute({ activeElement: input, querySelector: () => app } as unknown as Document);
});

test("the toolbar keeps the same history slots with and without history", () => {
  const count = (h: string) => (h.match(/<button/g) ?? []).length;
  const none = renderToStaticMarkup(<HistoryArrows back={false} forward={false} />);
  const some = renderToStaticMarkup(<HistoryArrows back forward={false} />);
  expect(count(none)).toBe(2);
  expect(count(some)).toBe(2);
  expect(none).toMatch(/<button[^>]*aria-hidden="true"/);
  expect(some).not.toMatch(/<button[^>]*aria-hidden="true"/);
});
