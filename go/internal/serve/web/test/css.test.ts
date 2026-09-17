import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";

const css = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");

test("the composer primary stays accent-filled while enabled (R3-J: disabled goes neutral)", () => {
  const rules = [...css.matchAll(/([^{}]*\.btn-primary[^{}]*)\{([^}]*)\}/g)].filter(([, sel]) => /\.composer/.test(sel) && !/:disabled/.test(sel));
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

// R3-J: visual polish.
const r3j = [...css.matchAll(/\/\* R3-J[^*]*\*\/([\s\S]*?)\/\* \/R3-J \*\//g)].map((m) => m[1]).join("\n");

test("R3-J: disabled Send is neutral grey, not dimmed accent", () => {
  expect(r3j).toMatch(/\.composer \.btn-primary:disabled\{[^}]*opacity:1[^}]*color:var\(--text-3\)[^}]*background:var\(--surface\)[^}]*border-color:var\(--line\)/);
});

test("R3-J: the user prompt is a raised bubble", () => {
  expect(r3j).toMatch(/\.prompt-bubble\{[^}]*background:var\(--raised\)[^}]*border-radius:12px[^}]*padding:8px 12px[^}]*max-width:min\(85%,672px\)/);
});

test("R3-J: New session matches .btn; hover eases and uses the overlay", () => {
  expect(r3j).toMatch(/\.side-new-label\{[^}]*border-radius:var\(--r-sm\)[^}]*font:500 var\(--fs-md\)/);
  expect(r3j).toMatch(/\.sel-btn,button\.more,\.side-nav-item,\.composer\{transition:[^}]*\.12s cubic-bezier\(\.2,\.8,\.2,1\)/);
  expect(r3j).toMatch(/\.side-nav-item:hover[^{]*\{background:var\(--hover\)\}/);
});

test("R3-J: Projects empty state sits at the top; page subtitle next to the title; −0 muted", () => {
  expect(r3j).toMatch(/\.proj-body>\.empty-state\{margin:0 auto\}/);
  expect(r3j).toMatch(/\.page-head \.head-main h1\{min-width:0\}/);
  expect(r3j).toMatch(/\.rt-del\.rt-zero\{color:var\(--text-3\)\}/);
});
