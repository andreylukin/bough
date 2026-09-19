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

const r3g = [...css.matchAll(/\/\* R3-G[^*]*\*\/([\s\S]*?)\/\* \/R3-G \*\//g)].map((m) => m[1]).join("\n");

test("R3-G: header groups use flex gaps, Mark seen matches 32px icon buttons, meters show a 2px floor", () => {
  expect(r3g).toMatch(/\.rt-bar>span\{min-width:2px\}/);
  expect(r3g).toMatch(/\.thread-head \.head-ack\{[^}]*height:32px[^}]*min-height:32px/);
  expect(r3g).toMatch(/\.rt-metrics\{[^}]*display:flex[^}]*flex-wrap:nowrap/);
  expect(r3g).not.toMatch(/position:absolute/);
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

test("R4-F: strip chips, the … summary and Edit into composer are at least 24px tall", () => {
  const block = css.slice(css.indexOf("/* R4-F */"));
  expect(block.indexOf("/* R4-F */")).toBe(0);
  expect(block).toMatch(/\.rt[,{][^}]*min-height:24px/);
  expect(block).toMatch(/\.rt-more>summary[^{]*\{[^}]*min-height:24px/);
  expect(block).toMatch(/\.prompt-acts>\.link[^{]*\{[^}]*min-height:24px/);
});

// MB-STREAM: streaming and waiting states.
const mbStream = [...css.matchAll(/\/\* MB-STREAM[^*]*\*\/([\s\S]*?)\/\* \/MB-STREAM \*\//g)].map((m) => m[1]).join("\n");

test("MB-STREAM: the block exists and the breathing dot fades without scaling", () => {
  expect(mbStream).toMatch(/\.breath-dot\{/);
  const frames = [...css.matchAll(/@keyframes breath-dot\{([^@]*?\})\}/g)].map((m) => m[1]).join("");
  expect(frames).not.toMatch(/scale|transform/);
  expect(css).not.toContain("typing-dots");
  expect(css.match(/\.turn-sending \.prompt-text\{/g)?.length).toBe(1);
});

test("MB-WORK: the narrow popover keeps the glyph column, so a failed glyph never sits on its title", () => {
  const work = css.match(/\/\* MB-WORK:[\s\S]*?\/\* \/MB-WORK \*\//)![0];
  expect(work).toMatch(/@container \(max-width:520px\)\{[^@]*\.work-popover \.work-row\{grid-template-columns:16px minmax\(0,1fr\) auto\}/);
});

test("MB-composer: a narrow desktop pane folds the mode badge to its icon so it never runs under Send", () => {
  const comp = css.match(/\/\* MB-composer:[\s\S]*?\/\* \/MB-composer \*\//)![0];
  expect(comp).toMatch(/@container \(max-width:600px\)\{[^@]*\.composer-tools \.mode-badge \.mode-word\{position:absolute;width:1px;height:1px;overflow:hidden;clip-path:inset\(50%\)/);
});

test("MB-composer: the foot's Start project session stays one line; the key hints give way instead", () => {
  const comp = css.match(/\/\* MB-composer:[\s\S]*?\/\* \/MB-composer \*\//)![0];
  expect(comp).toMatch(/\.composer-start\{white-space:nowrap;flex-shrink:0\}/);
  expect(comp).toMatch(/\.composer-foot>\.composer-local\{flex-shrink:0\}/);
  expect(comp).toMatch(/\.composer-foot>\.composer-hint\{min-width:0;overflow:hidden;flex-wrap:wrap;justify-content:flex-end;height:20px\}/);
});

test("MB-KEYS: key hints under the composer stay hidden on phones and touch, after the kbd restyle", () => {
  const keys = css.match(/\/\* MB-KEYS:[\s\S]*?\/\* \/MB-KEYS \*\//)![0];
  const at = keys.indexOf(".composer-hint{display:inline-flex");
  expect(at).toBeGreaterThan(-1);
  expect(keys.slice(at)).toMatch(/@media \(max-width:720px\),\(pointer:coarse\)\{\.composer-foot>\.composer-hint\{display:none\}\}/);
});

// The project page's two side columns used to be `display:none` below
// 1080px and 860px with nothing in their place, which took the
// MEMORY.md editor and every thread with them.
test("the project page's side columns become drawers when they will not fit", () => {
  const prj = /\/\* PROJECT-PAGE[\s\S]*?\/\* \/PROJECT-PAGE \*\//.exec(css)?.[0] ?? "";
  expect(prj).toBeTruthy();
  const tight = /@media \(max-width:1080px\)\{([\s\S]*?)\n\}/.exec(prj)?.[1] ?? "";
  expect(tight).toMatch(/\.prj-panel\{position:absolute/);
  expect(tight).not.toMatch(/\.prj-panel\{display:none\}/);
  const narrow = /@media \(max-width:860px\)\{([\s\S]*?)\n\}/.exec(prj)?.[1] ?? "";
  expect(narrow).toMatch(/\.prj-threads\[data-open\]\{display:flex;position:absolute/);
  // A drawer over the conversation is dismissable, and its toggle is shown.
  expect(prj).toMatch(/\.prj-scrim\{display:none;position:absolute/);
  expect(narrow).toMatch(/\.prj-threads-btn\{display:inline-flex\}/);
});
