/**
 * TUI palette: the terminal applies the stored theme (GET /theme — see
 * server/theme.ts). Hue tokens: green → accent (identity, active markers),
 * amber → warnings/holds, red → errors, blue → info, hairline → panel borders.
 * Surface tokens paint real backgrounds (ink 7 Box backgroundColor): bg → the
 * whole screen, panel → bordered containers, panelInset → the composer — so
 * surface presets like Midnight restyle the terminal wholesale. Colors render
 * as truecolor, both through ink (hex props) and the hand-rolled SGR line
 * renderer (fgParams). Tokens the server defines but applyTheme does not read
 * are simply inert here.
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
  hairline: "#666d79",
  bg: "#0e1013",
  panel: "#14161a",
  panelInset: "#1f2329",
  text: "#e7e9ed",
  text2: "#c9cdd4",
  muted: "#9aa1ac",
  // muted2 is text, not decoration: #656c77 measured 3.60:1 on bg and missed
  // WCAG AA; #7a828e clears it at 4.91:1 and still sits under muted (visual audit P3).
  muted2: "#7a828e",
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
  /** Primary foreground — the default for all text (components/Text.tsx). */
  text: string;
  /** Slightly-recessed prose. */
  text2: string;
  /** Secondary text: hints, metadata, folded summaries (replaces SGR dim). */
  muted: string;
  /** The most de-emphasized text — barely-there metadata. */
  muted2: string;
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
  text: FALLBACK.text,
  text2: FALLBACK.text2,
  muted: FALLBACK.muted,
  muted2: FALLBACK.muted2,
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
  palette.text = c.text ?? FALLBACK.text;
  palette.text2 = c.text2 ?? FALLBACK.text2;
  palette.muted = c.muted ?? FALLBACK.muted;
  palette.muted2 = c.muted2 ?? FALLBACK.muted2;
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
  // Ember/Rosewood accents sit near the reserved warn/error hues, so those
  // presets also move the colliding semantic token — accent, warn, and error
  // must stay three distinguishable hues (visual audit P2).
  { name: "Ember", note: "accent #d9a04f", colors: { green: "#d9a04f", amber: "#e6d47c" } },
  // Rosewood's old red #e2694a broke that invariant instead of fixing it: it sat
  // at 1.12 contrast against the accent (1.08 deuteranope-simulated), worse than
  // the Default palette it was correcting. #c85850 is a deeper brick that keeps
  // the warm identity, clears AA on bg (4.51:1) and separates from both accent
  // and amber (see theme.test.ts).
  { name: "Rosewood", note: "accent #d97a8e", colors: { green: "#d97a8e", red: "#c85850" } },
  { name: "Lagoon", note: "accent #3fbdb0", colors: { green: "#3fbdb0" } },
  { name: "Graphite", note: "accent #a7b5c8", colors: { green: "#a7b5c8" } },
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
      border: "#585e69",
      border2: "#464b54",
      border3: "#3b4048",
      hairline: "#636a76",
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
      // Borders lifted above the official highlight hexes (1.6–2.1:1 on base)
      // to clear ~3:1 for border/hairline; muted #6e6a86 anchors the ramp.
      border: "#6e6a86",
      border2: "#5a566f",
      border3: "#4c4a5d",
      hairline: "#7d7996",
      text: "#e0def4",
      text2: "#c8c5dd",
      muted: "#908caa",
      // Lifted off the official subtle hex (#6e6a86 = 3.03:1 on this base):
      // muted2 is text, so it owes AA 4.5 like every other preset's. 4.52:1,
      // still dimmer than muted (4.86:1) so the ramp keeps its order.
      muted2: "#8b86a8",
      green: "#c4a7e7",
      amber: "#f6c177",
      red: "#eb6f92",
      blue: "#9ccfd8",
    },
  },
];

/** hex → SGR truecolor background params ("48;2;r;g;b") for block surfaces. */
export function bgParams(hex: string): string {
  return fgParams(hex).replace(/^38/, "48");
}

/** hex → SGR truecolor foreground params ("38;2;r;g;b") for lines.ts. */
export function fgParams(hex: string): string {
  const h = hex.replace("#", "");
  const full = h.length === 3 ? h.split("").map((ch) => ch + ch).join("") : h.slice(0, 6);
  const n = parseInt(full, 16);
  return `38;2;${(n >> 16) & 255};${(n >> 8) & 255};${n & 255}`;
}
