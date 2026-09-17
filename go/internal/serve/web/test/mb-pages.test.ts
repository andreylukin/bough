import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");
const block = /\/\* MB-PAGES[^*]*\*\/([\s\S]*?)\/\* \/MB-PAGES \*\//.exec(css)?.[1] ?? "";

test("MB-PAGES: page heads are 56px with a 17px title, 52px on a phone", () => {
  expect(block).toMatch(/\.thread-head\.page-head,\.ov-head\{[^}]*height:56px/);
  expect(block).toMatch(/\.head-main h1,\.ov-head h1\{[^}]*font:600 var\(--fs-h2\)\/24px/);
  expect(block).toMatch(/@media \(max-width:720px\)\{\n  \.thread-head\.page-head,\.ov-head\{[^}]*min-height:52px/);
});

test("MB-PAGES: no bright control borders, no accent focus override, reduced motion covered", () => {
  expect(block).not.toContain("line-strong");
  expect(block).not.toMatch(/outline:2px solid var\(--accent\)/);
  expect(block).toMatch(/@media \(prefers-reduced-motion:reduce\)\{[^}]*\.chg-chev[^}]*transition:none/);
});
