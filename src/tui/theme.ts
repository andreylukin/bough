/**
 * The TUI palette, and the preview that lets you browse one without adopting it.
 *
 * THE INVARIANT THIS HOLDS: **browsing never commits.** Spec §16 is explicit — the
 * picker "previews live on cursor move and reverts on exit". So a preview is not a
 * different code path from an applied theme: `select()` paints the real palette, the
 * whole TUI recolors on the next render, and the *baseline* — whatever was in force
 * when the tab was entered — is held aside so `cancel()` can put it back byte for
 * byte. Only `commit()` moves the baseline. A preview implemented as "render this row
 * differently" would preview the swatch and not the product, which is the one thing a
 * theme picker exists to show.
 *
 * SECOND INVARIANT — **a theme is pure data.** A preset is a *partial* palette over a
 * fixed set of semantic tokens layered on the server's defaults; nothing here has a
 * component, a hard-coded hue, or a rebuild. That is what makes `palette` safe as a
 * mutable singleton rather than React state: every component reads it at render time
 * and `epoch` (bumped on each apply) is the dependency that invalidates a memoized
 * render such as the pre-wrapped transcript.
 *
 * THIRD — **one apply paints both renderers.** The TUI draws through two paths: ink's
 * `<Text color>` (hex) and the hand-rolled SGR line renderer in `format.ts` (parameter
 * bodies). `applyTheme` writes both, using `format.ts`'s own `setColors` hook, so a
 * theme cannot land in half the screen. `format.ts` deliberately does not import this
 * module — the dependency points this way, never back.
 *
 * KNOWN GAP, stated rather than faked: there is **no `/theme` route** yet
 * (`server/theme.ts` is T10.4) and therefore no `api.getTheme`/`putTheme`. Nothing in
 * this file fetches or persists; `ThemeState` is the shape the server will serve, and
 * `createThemePreview({persist})` takes the writer as an injected function so wiring
 * it later is one line in the composition root rather than an edit here. Until then a
 * committed theme lasts for the session, which is a visible limitation and not a
 * silent one.
 *
 * Ported from `src/tui/theme.ts` (palette, presets, SGR helpers). The preview
 * controller is new: the old tree scattered preview/revert across `App.tsx`'s key
 * handling, which is why "browsing never commits" was a convention rather than a
 * property.
 */
import { setColors } from "./format.ts";

// ---------------------------------------------------------------------------
// Wire shape
// ---------------------------------------------------------------------------

/** A partial palette: semantic token → hex. Tokens the TUI ignores are inert. */
export type ThemeColors = Record<string, string>;

/** What `GET /theme` will serve: the stored theme (if any) over the server defaults. */
export interface ThemeState {
  theme: { name: string; colors: ThemeColors } | null;
  defaults: ThemeColors;
}

/**
 * Mirror of the server's defaults for the tokens the TUI consumes. Present so a TUI
 * that cannot reach the server still paints a complete, contrast-checked palette
 * rather than terminal-default grey.
 */
export const FALLBACK: ThemeColors = {
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
  // muted2 is text, not decoration: the old #656c77 measured 3.60:1 on bg and missed
  // WCAG AA. #7a828e clears it at 4.91:1 and still reads below `muted`.
  muted2: "#7a828e",
};

// ---------------------------------------------------------------------------
// The live palette
// ---------------------------------------------------------------------------

export interface TuiPalette {
  /** Identity and active markers. */
  accent: string;
  /** Warnings and holds. */
  warn: string;
  error: string;
  info: string;
  /** Panel borders and hairline separators. */
  border: string;
  /** The screen background (root box). */
  bg: string;
  /** Bordered containers: the panel, cards, pickers. */
  panel: string;
  /** The composer's slightly-raised surface. */
  panelInset: string;
  /** Primary foreground. */
  text: string;
  /** Slightly-recessed prose. */
  text2: string;
  /** Hints, metadata, folded summaries. */
  muted: string;
  /** The most de-emphasized text. */
  muted2: string;
  /** Bumped on every `applyTheme` — a React dep for memoized renders. */
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

/** Resolve a state to the flat token map the palette is read from. */
export function resolveColors(state: ThemeState | null): ThemeColors {
  return { ...FALLBACK, ...(state?.defaults ?? {}), ...(state?.theme?.colors ?? {}) };
}

/**
 * Paint a theme. Mutates the singleton and pushes the same colors into `format.ts`'s
 * SGR parameters, so ink components and the raw line renderer never disagree.
 */
export function applyTheme(state: ThemeState | null): void {
  const c = resolveColors(state);
  palette.accent = c.green;
  palette.warn = c.amber;
  palette.error = c.red;
  palette.info = c.blue;
  palette.border = c.hairline;
  palette.bg = c.bg;
  palette.panel = c.panel;
  palette.panelInset = c.panelInset;
  palette.text = c.text;
  palette.text2 = c.text2;
  palette.muted = c.muted;
  palette.muted2 = c.muted2;
  palette.epoch++;
  setColors({
    muted: fgParams(palette.muted),
    accent: fgParams(palette.accent),
    warn: fgParams(palette.warn),
    error: fgParams(palette.error),
    info: fgParams(palette.info),
    surfaceBg: bgParams(palette.panelInset),
  });
}

/** hex → SGR truecolor foreground params (`38;2;r;g;b`) for `format.ts`. */
export function fgParams(hex: string): string {
  const h = hex.replace("#", "");
  const full = h.length === 3 ? h.split("").map((ch) => ch + ch).join("") : h.slice(0, 6);
  const n = Number.parseInt(full, 16);
  return `38;2;${(n >> 16) & 255};${(n >> 8) & 255};${n & 255}`;
}

/** hex → SGR truecolor background params (`48;2;r;g;b`) for block surfaces. */
export function bgParams(hex: string): string {
  return fgParams(hex).replace(/^38/, "48");
}

// ---------------------------------------------------------------------------
// Presets
// ---------------------------------------------------------------------------

export interface ThemePreset {
  name: string;
  /** The right-hand description on the row. */
  note: string;
  /** Partial: tokens omitted fall through to the server defaults. */
  colors: ThemeColors;
}

/**
 * The rows the theme tab lists. Most swap the single accent — bough is neutral-dark
 * plus one accent — while Midnight deepens the surfaces and Rosé Pine Moon is a full
 * third-party palette. "Default" is the empty partial: it resets to the built-ins.
 */
export const THEME_PRESETS: readonly ThemePreset[] = [
  { name: "Default", note: "built-in palette", colors: {} },
  { name: "Fjord", note: "accent #5c88c9", colors: { green: "#5c88c9" } },
  { name: "Iris", note: "accent #9a7fd1", colors: { green: "#9a7fd1" } },
  // Ember/Rosewood put the accent near the reserved warn/error hues, so each also
  // moves the token it collides with: accent, warn and error must stay three
  // distinguishable hues, or a warning reads as an ordinary highlight.
  { name: "Ember", note: "accent #d9a04f", colors: { green: "#d9a04f", amber: "#e6d47c" } },
  { name: "Rosewood", note: "accent #d97a8e", colors: { green: "#d97a8e", red: "#c85850" } },
  { name: "Lagoon", note: "accent #3fbdb0", colors: { green: "#3fbdb0" } },
  { name: "Graphite", note: "accent #a7b5c8", colors: { green: "#a7b5c8" } },
  {
    name: "Midnight",
    note: "deeper surfaces",
    colors: {
      bg: "#0a0b0e",
      panel: "#101216",
      panelInset: "#1a1e24",
      hairline: "#636a76",
    },
  },
  {
    // Roles mapped onto bough's tokens: base→bg, surface→panel, overlay→panelInset,
    // iris→accent (rose reads too warm as a primary), gold→amber, love→red,
    // foam→blue. Borders and `muted2` are lifted off the official hexes, which sit
    // at ~3:1 on this base — `muted2` is text and owes AA like every other preset's.
    name: "Rosé Pine Moon",
    note: "rosepinetheme.com",
    colors: {
      bg: "#232136",
      panel: "#2a273f",
      panelInset: "#393552",
      hairline: "#7d7996",
      text: "#e0def4",
      text2: "#c8c5dd",
      muted: "#908caa",
      muted2: "#8b86a8",
      green: "#c4a7e7",
      amber: "#f6c177",
      red: "#eb6f92",
      blue: "#9ccfd8",
    },
  },
];

/**
 * The swatch strip for one preset row: the surfaces first — near-identical dark presets
 * differ only there and need the wider cell — then the accent and the text. Resolved
 * from the preset's OWN colours, never the live palette: a row must look like itself
 * whether or not it is the theme currently painted.
 */
export function presetSwatch(p: ThemePreset): { token: string; color: string; block: string }[] {
  const c = resolveColors({ theme: { name: p.name, colors: p.colors }, defaults: {} });
  const surfaces = ["bg", "panel", "panelInset"];
  return [...surfaces, "green", "text"].map((token) => ({
    token,
    color: c[token],
    block: surfaces.includes(token) ? "███" : "██",
  }));
}

/** The preset a stored theme corresponds to, or -1 for a custom palette. */
export function presetIndex(state: ThemeState | null): number {
  const name = state?.theme?.name ?? "Default";
  return THEME_PRESETS.findIndex((p) => p.name === name);
}

/** A preset layered over the state's defaults — what `select()` paints. */
export function stateFor(base: ThemeState | null, preset: ThemePreset): ThemeState {
  const defaults = base?.defaults ?? {};
  // The empty partial IS the reset: no stored theme, defaults only (DELETE /theme).
  return preset.colors && Object.keys(preset.colors).length === 0
    ? { theme: null, defaults }
    : { theme: { name: preset.name, colors: preset.colors }, defaults };
}

// ---------------------------------------------------------------------------
// The preview controller
// ---------------------------------------------------------------------------

export interface ThemePreviewOptions {
  /** What is in force on entry — the baseline `cancel()` restores. */
  current?: ThemeState | null;
  /** Absent = the module's `applyTheme`. Injected so a test needs no terminal. */
  apply?: (state: ThemeState | null) => void;
  /**
   * Called by `commit()` with the adopted preset. Absent = the choice lasts for this
   * TUI session only — there is no `/theme` route to write to yet (see the header).
   * A rejected promise is swallowed: a failed save must not unpaint the screen.
   */
  persist?: (preset: ThemePreset, state: ThemeState) => unknown;
}

/**
 * One theme-tab browsing session.
 *
 * `cancel()` is idempotent and safe to call on any exit — closing the panel, jumping
 * to another tab, pressing escape — which is what lets `Panel.tsx` wire "leaving the
 * theme tab reverts" in one place instead of at every exit key.
 */
export interface ThemePreview {
  readonly presets: readonly ThemePreset[];
  /** Cursor row. Starts on the theme in force, or 0 for a custom palette. */
  readonly index: number;
  /** True while a preview is painted that the user has not kept. */
  readonly previewing: boolean;
  /** The name of the theme currently painted. */
  readonly name: string;
  /** Move the cursor and preview what it lands on. Clamped, never wraps. */
  move(delta: number): void;
  select(index: number): void;
  /** Keep what is painted: the baseline moves and `persist` (if any) is called. */
  commit(): void;
  /** Restore the baseline. No-op when nothing is being previewed. */
  cancel(): void;
}

export function createThemePreview(options: ThemePreviewOptions = {}): ThemePreview {
  const apply = options.apply ?? applyTheme;
  const presets = THEME_PRESETS;
  let baseline: ThemeState | null = options.current ?? null;
  let index = Math.max(0, presetIndex(baseline));
  let previewing = false;

  const paint = (i: number): void => {
    index = i;
    const next = stateFor(baseline, presets[i]);
    previewing = (next.theme?.name ?? "Default") !== (baseline?.theme?.name ?? "Default");
    apply(next);
  };

  return {
    presets,
    get index() {
      return index;
    },
    get previewing() {
      return previewing;
    },
    get name() {
      return presets[index]?.name ?? "Default";
    },
    select(i: number) {
      if (i < 0 || i >= presets.length || i === index) return;
      paint(i);
    },
    move(delta: number) {
      const i = Math.min(presets.length - 1, Math.max(0, index + delta));
      if (i !== index) paint(i);
    },
    commit() {
      const state = stateFor(baseline, presets[index]);
      baseline = state;
      previewing = false;
      apply(state);
      if (!options.persist) return;
      // Fire-and-forget: persistence is a write-behind, never a gate on the paint.
      try {
        Promise.resolve(options.persist(presets[index], state)).catch(() => {});
      } catch {
        // A synchronous throw is the same non-event as a rejected promise.
      }
    },
    cancel() {
      if (!previewing) return;
      previewing = false;
      index = Math.max(0, presetIndex(baseline));
      apply(baseline);
    },
  };
}
