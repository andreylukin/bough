// Semantic tokens for the whole UI. One accent (green), amber reserved for
// pending/hold-and-ask, red for deny/danger. Everything else neutral-dark.
// Values are CSS variables so a user theme can swap the palette at runtime:
// defaults live in global.css :root, overrides are applied by applyTheme()
// (loaded from the server's /theme — see the /theme skill). The server keeps a
// mirror of the defaults in src/server/theme.ts.
export const c = {
  bg: "var(--bough-bg)",
  panel: "var(--bough-panel)",
  panel2: "var(--bough-panel2)",
  panel3: "var(--bough-panel3)",
  panelInset: "var(--bough-panelInset)",
  canvas: "var(--bough-canvas)",
  border: "var(--bough-border)",
  border2: "var(--bough-border2)",
  border3: "var(--bough-border3)",
  hairline: "var(--bough-hairline)",
  text: "var(--bough-text)",
  text2: "var(--bough-text2)",
  muted: "var(--bough-muted)",
  muted2: "var(--bough-muted2)",
  green: "var(--bough-green)",
  amber: "var(--bough-amber)",
  red: "var(--bough-red)",
  blue: "var(--bough-blue)",
} as const;

/** Token color at `pct`% opacity (replaces rgba() literals, works with var()). */
export const alpha = (color: string, pct: number) =>
  `color-mix(in srgb, ${color} ${pct}%, transparent)`;

/** `pct`% of `a` blended into `b` (replaces hand-picked tint/shade hexes). */
export const mix = (a: string, b: string, pct: number) =>
  `color-mix(in srgb, ${a} ${pct}%, ${b})`;

/** Apply a saved theme's colors as CSS-variable overrides (missing keys = default). */
export function applyTheme(colors: Record<string, string> | null | undefined): void {
  const root = document.documentElement;
  for (const token of Object.keys(c)) {
    const value = colors?.[token];
    if (value) root.style.setProperty(`--bough-${token}`, value);
    else root.style.removeProperty(`--bough-${token}`);
  }
}

export interface ThemePreset {
  name: string;
  colors: Record<string, string>;
}

/**
 * Built-in palettes for the composer's /theme picker ("Default" is the absence
 * of a theme, special-cased there). Official Rosé Pine values (rose-pine/palette)
 * mapped onto the token contract; custom palettes come from the /theme skill.
 */
export const THEME_PRESETS: ThemePreset[] = [
  {
    name: "Rosé Pine",
    colors: {
      bg: "#191724",
      panel: "#1f1d2e",
      panel2: "#211f31",
      panel3: "#232135",
      panelInset: "#26233a",
      canvas: "#16141f",
      border: "#524f67",
      border2: "#403d52",
      border3: "#26233a",
      hairline: "#524f67",
      text: "#e0def4",
      text2: "#c5c2dd",
      muted: "#908caa",
      muted2: "#6e6a86",
      green: "#9ccfd8",
      amber: "#f6c177",
      red: "#eb6f92",
      blue: "#31748f",
    },
  },
  {
    name: "Rosé Pine Moon",
    colors: {
      bg: "#232136",
      panel: "#2a273f",
      panel2: "#2c2942",
      panel3: "#2e2b46",
      panelInset: "#393552",
      canvas: "#201e31",
      border: "#56526e",
      border2: "#44415a",
      border3: "#393552",
      hairline: "#56526e",
      text: "#e0def4",
      text2: "#c6c3de",
      muted: "#908caa",
      muted2: "#6e6a86",
      green: "#9ccfd8",
      amber: "#f6c177",
      red: "#eb6f92",
      blue: "#3e8fb0",
    },
  },
];

export const mono = "'IBM Plex Mono', ui-monospace, monospace";
export const sans = "'IBM Plex Sans', system-ui, sans-serif";
