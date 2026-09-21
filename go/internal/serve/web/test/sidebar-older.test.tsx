import { afterAll, beforeAll, expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { Sidebar } from "../src/app";
import type { Row } from "../src/types";

// Sidebar reads window.navigation at mount; a bare object is enough for a static
// render. It goes away after this file: other tests render with no window at all.
const g = globalThis as unknown as { window?: object };
beforeAll(() => { g.window ??= {}; });
afterAll(() => { delete g.window; });

const old = new Date(Date.now() - 10 * 86_400_000).toISOString();
const rows = Array.from({ length: 12 }, (_, i) => ({
  id: `s${i}`, cwd: "/r/one", repo: "/r/one", status: "idle", lastAt: old, title: `tell me about ${i}`,
})) as unknown as Row[];

function render(query: string) {
  return renderToStaticMarkup(<Sidebar rows={rows} selected={null} onSelect={() => {}} query={query} onQuery={() => {}}
    showArchived={false} onToggleArchived={() => {}} />);
}

test("the group eyebrow is text only, no icon or mark before the name", () => {
  const html = render("");
  expect(html).toMatch(/class="ws-head"[^>]*><span class="ws-fold"[^>]*><svg[^]*?<\/svg><\/span><span class="ws-name/);
  expect(html).not.toContain("ws-mark");
});

test("a long expansion of older sessions folds from its foot as well", () => {
  // Under a search every fold is open, so the older rows are expanded.
  const html = render("tell");
  expect(html.match(/class="ws-older"/g)?.length).toBe(2);
  expect(html).toContain("Hide older");
});

test("the search field is named the same as the toolbar: Filter", () => {
  expect(render("tell")).toContain('placeholder="Filter sessions"');
});
