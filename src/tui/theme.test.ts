import assert from "node:assert/strict";
import { test } from "bun:test";
import { applyTheme, palette, THEME_PRESETS } from "./theme.ts";

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
