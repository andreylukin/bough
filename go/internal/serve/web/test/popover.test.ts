import { expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { anchorPlace, clampShift } from "../src/popover";

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

const pane = { left: 303, right: 1440, top: 0, bottom: 900 };

test("the composer's model picker flips to open rightward instead of over the sidebar", () => {
  // 03-model-picker: trigger 458–589 at the bottom, 360 wide, end-aligned went to x=229.
  const p = anchorPlace({ left: 458, right: 589, top: 827, bottom: 855 }, 360, 450, pane, "end");
  expect(p.left).toBe(458);
  expect(p.up).toBe(true);
  expect(p.top + Math.min(450, p.maxHeight)).toBe(823);
});

test("an end-aligned popover that fits keeps the trigger's right edge", () => {
  const p = anchorPlace({ left: 957, right: 1073, top: 10, bottom: 34 }, 560, 200, pane, "end");
  expect(p.left + 560).toBe(1073);
  expect(p.top).toBe(38);
  expect(p.up).toBe(false);
});

test("too tall for either side: it opens on the roomier side and scrolls inside", () => {
  const p = anchorPlace({ left: 957, right: 1073, top: 10, bottom: 34 }, 560, 2000, pane, "end");
  expect(p.up).toBe(false);
  expect(p.maxHeight).toBe(900 - 8 - 34 - 4);
});

test("fits neither edge: clamped inside the bounds with the margin", () => {
  const p = anchorPlace({ left: 20, right: 60, top: 10, bottom: 34 }, 355, 100, { left: 0, right: 375, top: 0, bottom: 812 }, "start");
  expect(p.left).toBe(375 - 8 - 355);
  const q = anchorPlace({ left: 20, right: 60, top: 10, bottom: 34 }, 500, 100, { left: 0, right: 375, top: 0, bottom: 812 }, "end");
  expect(q.left).toBe(8);
});
