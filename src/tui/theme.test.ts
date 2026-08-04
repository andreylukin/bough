import assert from "node:assert/strict";
import { test } from "bun:test";
import { colors, UI } from "./format.ts";
import {
  applyTheme,
  createThemePreview,
  fgParams,
  palette,
  setBackgroundPainter,
  subscribeTheme,
  THEME_PRESETS,
  themeEpoch,
} from "./theme.ts";

/** Relative luminance, WCAG 2.1 §relative-luminance. */
function luminance(hex: string): number {
  const n = hex.replace("#", "");
  const ch = [0, 2, 4].map((i) => parseInt(n.slice(i, i + 2), 16) / 255);
  const lin = ch.map((c) => (c <= 0.03928 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * lin[0]! + 0.7152 * lin[1]! + 0.0722 * lin[2]!;
}

function contrast(a: string, b: string): number {
  const [x, y] = [luminance(a), luminance(b)].sort((p, q) => q - p);
  return (x! + 0.05) / (y! + 0.05);
}

test("the drag selection is legible in every preset", () => {
  // THE BUG THIS EXISTS FOR: the selection overlay used `INVERSE`, which needs
  // something to invert — the overlay is a fresh renderable with no colours of its
  // own, so OpenTUI resolved BOTH sides to white and every selected cell came out
  // #ffffff on #ffffff. The text you were dragging over disappeared while you
  // dragged over it, in the transcript and in every panel tab.
  //
  // It reported `inverse: true` the whole time, which is exactly why counting the
  // attribute was not enough: the guard has to be about what is READABLE.
  // Through `applyTheme`, not `resolveColors`: a preset carries a PARTIAL palette
  // and the gaps are filled at apply time, so reading the partial would test colours
  // the screen never shows.
  try {
    for (const preset of THEME_PRESETS) {
      applyTheme({ theme: { name: preset.name, colors: preset.colors }, defaults: {} });
      const ratio = contrast(palette.bg, palette.accent);
      assert.ok(
        ratio >= 4.5,
        `${preset.name}: selection is ${ratio.toFixed(2)}:1 ` +
          `(${palette.bg} on ${palette.accent}) — WCAG AA for text is 4.5:1`,
      );
    }
  } finally {
    applyTheme(null); // the palette is module state; leave it as found
  }
});

// ---------------------------------------------------------------------------
// One apply reaches every path
// ---------------------------------------------------------------------------

test("applying a theme paints the screen background, not just the palette field", () => {
  const painted: string[] = [];
  try {
    // Registering paints immediately: the theme is fetched and applied BEFORE the
    // renderer exists (`tui/main.tsx`), so a painter that waited for the next apply
    // would leave the user's own stored theme as the one theme never painted.
    setBackgroundPainter((hex) => painted.push(hex));
    assert.equal(painted.length, 1);

    const midnight = THEME_PRESETS.find((p) => p.name === "Midnight")!;
    applyTheme({ theme: { name: midnight.name, colors: midnight.colors }, defaults: {} });
    assert.equal(painted.at(-1), midnight.colors.bg);
    assert.equal(palette.bg, midnight.colors.bg);

    setBackgroundPainter(null);
    const before = painted.length;
    applyTheme(null);
    // Deregistered: `bough exec` and every test apply themes with no renderer, and
    // painting into a torn-down one is the crash this avoids.
    assert.equal(painted.length, before);
  } finally {
    setBackgroundPainter(null);
    applyTheme(null);
  }
});

test("the component palette moves with the theme, not just the SGR one", () => {
  try {
    // THE BUG THIS EXISTS FOR: `UI` was a frozen map of ANSI NAMES, so ~20 component
    // call sites painted the terminal's own green/yellow/red no matter what theme was
    // in force — the transcript wore Rosé Pine and the composer's border beside it
    // stayed terminal-green. One screen, two palettes.
    const rose = THEME_PRESETS.find((p) => p.name === "Rosé Pine Moon")!;
    applyTheme({ theme: { name: rose.name, colors: rose.colors }, defaults: {} });
    assert.equal(UI.accent, rose.colors.green);
    assert.equal(UI.warn, rose.colors.amber);
    assert.equal(UI.error, rose.colors.red);
    // …and the SGR path agrees with it, since both are written by the same apply.
    assert.equal(colors.accent, fgParams(rose.colors.green!));
  } finally {
    applyTheme(null);
  }
});

test("every apply bumps the epoch and notifies subscribers", () => {
  const seen: number[] = [];
  const stop = subscribeTheme(() => seen.push(themeEpoch()));
  try {
    const before = themeEpoch();
    applyTheme(null);
    assert.deepEqual(seen, [before + 1]);
    // The epoch a listener reads is the one AFTER the apply — a subscriber that
    // re-renders must never see a half-applied palette.
    assert.equal(seen[0], themeEpoch());

    stop();
    applyTheme(null);
    assert.equal(seen.length, 1, "unsubscribed listeners stop hearing");
  } finally {
    stop();
    applyTheme(null);
  }
});

test("previewing a preset notifies, so a repaint has something to hang off", () => {
  // The picker mutates a singleton and the panel's `move` returns its state
  // unchanged, so without this notification React had no reason to re-render and the
  // live preview repainted nothing on an idle TUI.
  const bumps: number[] = [];
  const stop = subscribeTheme(() => bumps.push(themeEpoch()));
  try {
    const preview = createThemePreview({ current: null });
    preview.move(1);
    assert.equal(bumps.length, 1);
    preview.cancel();
    assert.equal(bumps.length, 2);
  } finally {
    stop();
    applyTheme(null);
  }
});
