import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";

const css = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");
const r4j = [...css.matchAll(/\/\* R4-J[^*]*\*\/([\s\S]*?)\/\* \/R4-J \*\//g)].map((m) => m[1]).join("\n");

test("R4-J: icon controls inherit the font and ease their colours", () => {
  for (const sel of [".back", ".copy-btn", ".more", ".row-twist", ".side-icon"]) {
    expect(r4j).toMatch(new RegExp(`[^{}]*\\${sel}(?=[,{])[^{}]*\\{[^}]*font:inherit`));
  }
  for (const sel of [".side-icon", ".side-new", ".back", ".row-twist", ".sec-fold", ".ws-head", ".chg-scope"]) {
    expect(r4j).toMatch(new RegExp(`[^{}]*\\${sel}(?=[,{])[^{}]*\\{[^}]*transition-property:background-color,color,border-color;transition-duration:var\\(--dur-fast\\);transition-timing-function:var\\(--ease-out\\)`));
  }
  expect(css).not.toMatch(/var\(--r-8\)/);
});

test("R4-J: hooks state pills are sentence case and muted, no underlined session link", () => {
  expect(r4j).toMatch(/\.hk2-state\{[^}]*text-transform:none/);
  expect(r4j).toMatch(/\.hk-session\{[^}]*text-decoration:none/);
});

test("R4-J: a passed hook wears a check, not a hollow circle", async () => {
  const { StateWord } = await import("../src/hooks");
  const html = renderToStaticMarkup(<StateWord word="Passed" />);
  expect(html).toContain("M4 12.5l5 5L20 6.5");
  expect(html).not.toContain("<circle");
});

test("R4-J: the thin tool row meta has no leading separator", () => {
  expect(r4j).toMatch(/\.block\.thin>summary \.tool-meta::before[^{]*\{content:none\}/);
});
