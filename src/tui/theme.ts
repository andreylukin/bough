/**
 * TUI palette: the terminal consumes the same stored theme as the web UI
 * (GET /theme — see server/theme.ts). Hue tokens: green → accent (identity,
 * active markers), amber → warnings/holds, red → errors, blue → info,
 * hairline → panel borders. Surface tokens paint real backgrounds (ink 7 Box
 * backgroundColor): bg → the whole screen, panel → bordered containers,
 * panelInset → the composer — so surface presets like Midnight restyle the
 * terminal, not just the web. Colors render as truecolor, both through ink
 * (hex props) and the hand-rolled SGR line renderer (fgParams).
 *
 * A mutable singleton, not React state: every component reads `palette` at
 * render time, and `epoch` (bumped on each apply) is the dependency that
 * invalidates memoized renders like the pre-wrapped conversation lines.
 */
import { api, type ThemeState } from "./api.ts";

/** Mirror of the server's THEME_DEFAULTS for the tokens the TUI consumes. */
const FALLBACK = {
  green: "#4ec98f",
  amber: "#d9b45f",
  red: "#e2776e",
  blue: "#5c88c9",
  hairline: "#3a414c",
  bg: "#0e1013",
  panel: "#14161a",
  panelInset: "#1f2329",
};

export interface TuiPalette {
  accent: string;
  warn: string;
  error: string;
  info: string;
  border: string;
  /** The screen background (root box) — the terminal's own bg is painted over. */
  bg: string;
  /** Bordered containers: pickers, panels, cards. */
  panel: string;
  /** The composer's slightly-raised surface. */
  panelInset: string;
  /** Bumped on every applyTheme — a React dep for memoized renders. */
  epoch: number;
}

export const palette: TuiPalette = {
  accent: FALLBACK.green,
  warn: FALLBACK.amber,
  error: FALLBACK.red,
  info: FALLBACK.blue,
  border: FALLBACK.hairline,
  bg: FALLBACK.bg,
  panel: FALLBACK.panel,
  panelInset: FALLBACK.panelInset,
  epoch: 0,
};

export function applyTheme(state: ThemeState | null): void {
  const c = { ...(state?.defaults ?? {}), ...(state?.theme?.colors ?? {}) };
  palette.accent = c.green ?? FALLBACK.green;
  palette.warn = c.amber ?? FALLBACK.amber;
  palette.error = c.red ?? FALLBACK.red;
  palette.info = c.blue ?? FALLBACK.blue;
  palette.border = c.hairline ?? FALLBACK.hairline;
  palette.bg = c.bg ?? FALLBACK.bg;
  palette.panel = c.panel ?? FALLBACK.panel;
  palette.panelInset = c.panelInset ?? FALLBACK.panelInset;
  palette.epoch++;
}

/** Fetch + apply the stored theme; silent on failure (defaults stay). */
export async function loadTheme(): Promise<void> {
  try {
    applyTheme(await api.getTheme());
  } catch {
    // server unreachable or /theme errored — the default palette applies
  }
}

/**
 * Presets for the theme tab (Panel.tsx). Partial palettes over the server's
 * semantic tokens: most swap the single accent (`green` — bough is neutral-dark
 * + one accent); Midnight deepens the surfaces; Rosé Pine Moon is the full
 * third-party palette (rosepinetheme.com moon variant, official hexes; iris as
 * the accent). "Default" resets to the built-in palette (DELETE /theme).
 */
export interface ThemePreset {
  name: string;
  /** Right-hand description on the row. */
  note: string;
  colors: Record<string, string>;
}

export const THEME_PRESETS: ThemePreset[] = [
  { name: "Default", note: "built-in palette", colors: {} },
  { name: "Fjord", note: "accent #5c88c9", colors: { green: "#5c88c9" } },
  { name: "Iris", note: "accent #9a7fd1", colors: { green: "#9a7fd1" } },
  { name: "Ember", note: "accent #d9a04f", colors: { green: "#d9a04f" } },
  { name: "Rosewood", note: "accent #d97a8e", colors: { green: "#d97a8e" } },
  { name: "Lagoon", note: "accent #3fbdb0", colors: { green: "#3fbdb0" } },
  { name: "Graphite", note: "accent #aeb4bd", colors: { green: "#aeb4bd" } },
  {
    name: "Midnight",
    note: "deeper surfaces",
    colors: {
      bg: "#0a0b0e",
      canvas: "#0c0d11",
      panel: "#101216",
      panel2: "#12151a",
      panel3: "#15181d",
      panelInset: "#1a1e24",
      border: "#262b33",
      border2: "#1f242b",
      border3: "#181c22",
    },
  },
  {
    // Roles mapped onto bough's tokens: base→bg, surface→panel, overlay→
    // panelInset, highlights→borders, iris→accent (rose reads too warm as a
    // primary), gold→amber, love→red, foam→blue, subtle/muted→muted/muted2.
    name: "Rosé Pine Moon",
    note: "rosepinetheme.com",
    colors: {
      bg: "#232136",
      canvas: "#2a283e",
      panel: "#2a273f",
      panel2: "#2e2b44",
      panel3: "#322f49",
      panelInset: "#393552",
      border: "#44415a",
      border2: "#393552",
      border3: "#2a283e",
      hairline: "#56526e",
      text: "#e0def4",
      text2: "#c8c5dd",
      muted: "#908caa",
      muted2: "#6e6a86",
      green: "#c4a7e7",
      amber: "#f6c177",
      red: "#eb6f92",
      blue: "#9ccfd8",
    },
  },
];

/** hex → SGR truecolor foreground params ("38;2;r;g;b") for lines.ts. */
export function fgParams(hex: string): string {
  const h = hex.replace("#", "");
  const full = h.length === 3 ? h.split("").map((ch) => ch + ch).join("") : h.slice(0, 6);
  const n = parseInt(full, 16);
  return `38;2;${(n >> 16) & 255};${(n >> 8) & 255};${n & 255}`;
}
