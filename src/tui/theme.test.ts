import { assert } from "jsr:@std/assert@1";
import { applyTheme, palette, THEME_PRESETS } from "./theme.ts";

/**
 * Accent/warn/error must stay three distinguishable hues in EVERY preset — the
 * invariant Rosewood shipped violating (its red sat 1.12 contrast from its
 * accent). This pins it so the next colliding preset fails here instead.
 *
 * Metric: CIELAB ΔE76 between the colors after a Vienot deuteranope
 * simulation. Deuteranopia is the real motivation, and plain WCAG contrast is
 * NOT a usable proxy across presets: it only sees luminance, so Lagoon's teal
 * accent vs red scores 1.01 simulated while being obviously distinct, and any
 * floor low enough to pass that is too low to catch anything.
 */
function chan(hex: string): number[] {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 255, (n >> 8) & 255, n & 255].map((v) => {
    const c = v / 255;
    return c <= 0.03928 ? c / 12.92 : Math.pow((c + 0.055) / 1.055, 2.4);
  });
}

/** WCAG relative-luminance contrast, used for the AA checks on text tokens. */
function contrast(a: string, b: string): number {
  const l = (hex: string) => {
    const [r, g, bl] = chan(hex);
    return 0.2126 * r + 0.7152 * g + 0.0722 * bl;
  };
  const [x, y] = [l(a), l(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}

/** Vienot 1999 deuteranope simulation (LMS projection), back to sRGB hex. */
function deuteranope(hex: string): string {
  const [r, g, b] = chan(hex);
  const L = 0.31399022 * r + 0.63951294 * g + 0.04649755 * b;
  const S = 0.01775239 * r + 0.10944209 * g + 0.87256922 * b;
  const M = 0.9513092 * L + 0.04866992 * S;
  const out = [
    5.47221206 * L - 4.6419601 * M + 0.16963708 * S,
    -1.1252419 * L + 2.29317094 * M - 0.1678952 * S,
    0.02980165 * L - 0.19318073 * M + 1.16364789 * S,
  ].map((c) => {
    const v = Math.max(0, Math.min(1, c));
    const s = v <= 0.0031308 ? v * 12.92 : 1.055 * Math.pow(v, 1 / 2.4) - 0.055;
    return Math.round(s * 255).toString(16).padStart(2, "0");
  });
  return "#" + out.join("");
}

function lab(hex: string): number[] {
  const [r, g, b] = chan(hex);
  const xyz = [
    (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.95047,
    0.2126 * r + 0.7152 * g + 0.0722 * b,
    (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.08883,
  ].map((t) => (t > 0.008856 ? Math.cbrt(t) : 7.787 * t + 16 / 116));
  return [116 * xyz[1] - 16, 500 * (xyz[0] - xyz[1]), 200 * (xyz[1] - xyz[2])];
}

/** ΔE76 between two colors as a deuteranope sees them. */
function separation(a: string, b: string): number {
  const [x, y] = [lab(deuteranope(a)), lab(deuteranope(b))];
  return Math.hypot(x[0] - y[0], x[1] - y[1], x[2] - y[2]);
}

// Floor picked from what the currently-correct presets actually achieve: the
// tightest passing pair is Ember's accent/warn at 15.0, everything else is
// ≥18. Rosewood as shipped scored 12.6 (amber/red) — below this line.
const MIN_SEPARATION = 14;

Deno.test("THEME_PRESETS: accent/warn/error stay distinguishable in every preset", () => {
  for (const preset of THEME_PRESETS) {
    // Resolve through the real code path so partial presets inherit defaults.
    applyTheme({ theme: { name: preset.name, colors: preset.colors }, tokens: [], defaults: {} });
    const pairs: [string, string, string][] = [
      ["accent/warn", palette.accent, palette.warn],
      ["accent/error", palette.accent, palette.error],
      ["warn/error", palette.warn, palette.error],
    ];
    for (const [label, a, b] of pairs) {
      const d = separation(a, b);
      assert(
        d >= MIN_SEPARATION,
        `${preset.name} ${label} (${a} vs ${b}) separation ${d.toFixed(1)} < ${MIN_SEPARATION}`,
      );
    }
  }
  applyTheme(null);
});

Deno.test("palette: text tokens clear WCAG AA in EVERY preset", () => {
  // Checking only the fallback let a shipped preset miss AA unnoticed: Rosé
  // Pine Moon overrode muted2 to #6e6a86 (3.03:1 on its base) while the
  // fallback passed. A preset that overrides a text token owes the same bar.
  for (const preset of [null, ...THEME_PRESETS]) {
    // Resolve through the real code path so partial presets inherit defaults.
    applyTheme(
      preset && { theme: { name: preset.name, colors: preset.colors }, tokens: [], defaults: {} },
    );
    const name = preset?.name ?? "fallback";
    // muted2 is the most de-emphasized TEXT token, so 4.5:1 applies (border is
    // decorative and only needs 3:1 — deliberately not checked here).
    for (const token of ["text", "text2", "muted", "muted2"] as const) {
      const c = contrast(palette[token], palette.bg);
      assert(
        c >= 4.5,
        `${name}: ${token} ${palette[token]} on bg ${palette.bg} is ${c.toFixed(2)}:1`,
      );
    }
    // The ramp stays ordered: each step is dimmer than the one above it.
    assert(
      contrast(palette.muted2, palette.bg) < contrast(palette.muted, palette.bg),
      `${name}: muted2 is not dimmer than muted`,
    );
  }
  applyTheme(null);
});
