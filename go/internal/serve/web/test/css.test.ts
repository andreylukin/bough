import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");

test("the composer primary stays accent-filled, disabled included", () => {
  const rules = [...css.matchAll(/([^{}]*\.btn-primary[^{}]*)\{([^}]*)\}/g)].filter(([, sel]) => /\.composer/.test(sel));
  expect(rules.length).toBeGreaterThan(0);
  for (const [, , body] of rules) expect(body).not.toMatch(/background:var\(--(surface|raised|bg)\)/);
});

// R2-J: motion system and type/spacing polish.
const r2j = [...css.matchAll(/\/\* R2-J[^*]*\*\/([\s\S]*?)\/\* \/R2-J \*\//g)].map((m) => m[1]).join("\n");

test("R2-J: popovers and dialogs enter with motion, right-anchored ones grow from their corner", () => {
  expect(r2j).toMatch(/\.sel-pop[^{]*\{[^}]*animation:pop-in/);
  expect(r2j).toMatch(/\n\.dlg\{animation:pop-in/);
  expect(r2j).toMatch(/\.head-pop,\.work-popover,\.sel-end\{transform-origin:top right\}/);
});

test("R2-J: .btn and .row ease their hover in 120-180ms, and reduced motion turns it all off", () => {
  expect(r2j).toMatch(/\.btn[^{]*\{[^}]*transition-property:[^;}]*background-color[^}]*transition-duration:var\(--dur-fast\)/);
  expect(r2j).toMatch(/\.row[^{]*\{[^}]*transition:background-color var\(--dur-fast\) var\(--ease-out\)/);
  expect(r2j).toMatch(/@media \(prefers-reduced-motion:reduce\)\{[^@]*animation:none/);
});

test("R2-J: one h1 token; composer at prose size; no --fs-14/--fs-16 outside the named scale", () => {
  expect(r2j).toMatch(/\.head-main h1\{font-size:var\(--fs-h1\)/);
  expect(r2j).toMatch(/\.composer textarea\{font:var\(--fs-base\)/);
  expect(css).not.toMatch(/--fs-14|--fs-16/);
});

test("R2-J: spacing — turn-foot gap, hooks columns, no stray phone dot, wiki empty measure", () => {
  expect(r2j).toMatch(/\.turn-foot\{gap:4px var\(--s-2\)\}/);
  expect(r2j).toMatch(/\.hk-main>\.hk-dec::before\{content:none\}/);
  expect(r2j).toMatch(/\.empty-state p\{max-width:60ch\}/);
});
