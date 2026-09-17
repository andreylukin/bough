import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { clampShift } from "../src/popover";

test("a popover inside the viewport stays put", () => {
  expect(clampShift(100, 500, 1280)).toBe(0);
});

test("a popover past the right edge shifts left to an 8px margin", () => {
  // The edits popover at 1280: anchored at 1056, 760 wide.
  expect(clampShift(1056, 1816, 1280)).toBe(1272 - 1816);
});

test("a popover past the left edge shifts right to an 8px margin", () => {
  expect(clampShift(-20, 300, 1280)).toBe(28);
});

test("a popover wider than the viewport keeps its left edge on screen", () => {
  expect(clampShift(442, 1186, 700)).toBe(8 - 442);
});

const css = readFileSync(new URL("../dist/index.html", import.meta.url), "utf8");

test("selected is a clearly stronger mix than hover, and row hover needs a real pointer", () => {
  const mix = (name: string) => Number(new RegExp(`--${name}:color-mix\\(in srgb,var\\(--text-1\\) (\\d+)%`).exec(css)?.[1]);
  expect(mix("sel") - mix("hover")).toBeGreaterThanOrEqual(4);
  expect(css).toMatch(/@media \(hover:hover\)\{\.row:hover\{background:var\(--hover\)\}\}/);
});

test("header chips get a 24px hit area", () => {
  expect(css).toMatch(/\.runtime-strip>\.rt-jobs>summary[^{]*\{[^}]*padding-block:4px;margin-block:-4px/);
});
