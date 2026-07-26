/**
 * UI theme: a named partial palette over the semantic tokens below, persisted at
 * ~/.bough/theme.json and served via GET/PUT/DELETE /theme. The TUI fetches it on
 * boot (src/tui/theme.ts) and paints the tokens it consumes as truecolor — so a
 * theme is pure data, no rebuild. THEME_DEFAULTS is the built-in palette, kept
 * server-side so the /theme skill can ground drafts in the real defaults.
 *
 * The token list is a fixed contract and stays wider than what the TUI reads
 * today: it is what PUT /theme validates against and what the skill documents.
 */
import { z } from "zod/v4";
import { join } from "node:path";
import { boughHome } from "../paths.ts";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";

/** The semantic tokens a theme may set. Fixed contract. */
export const THEME_TOKENS = [
  "bg",
  "panel",
  "panel2",
  "panel3",
  "panelInset",
  "canvas",
  "border",
  "border2",
  "border3",
  "hairline",
  "text",
  "text2",
  "muted",
  "muted2",
  "green",
  "amber",
  "red",
  "blue",
] as const;
export type ThemeToken = (typeof THEME_TOKENS)[number];

/** The default (built-in) palette. */
export const THEME_DEFAULTS: Record<ThemeToken, string> = {
  bg: "#0e1013",
  panel: "#14161a",
  panel2: "#161a1f",
  panel3: "#191c21",
  panelInset: "#1f2329",
  canvas: "#111318",
  // Borders sit ≥3:1 against bg (hairline higher — it outlines the TUI's
  // panels); border2/border3 step down for layering. Visual audit 2026-07-21:
  // the old set (#2b3038/#3a414c) measured 1.4–1.9:1 — invisible on many
  // displays.
  border: "#5a616c",
  border2: "#484e57",
  border3: "#3c4149",
  hairline: "#666d79",
  text: "#e7e9ed",
  text2: "#c9cdd4",
  muted: "#9aa1ac",
  // Keep in sync with FALLBACK in src/tui/theme.ts: this is the value that
  // actually reaches the TUI when the server is up, so leaving the old 3.60:1
  // hex here would have quietly undone the AA fix.
  muted2: "#7a828e",
  green: "#4ec98f",
  amber: "#d9b45f",
  red: "#e2776e",
  blue: "#5c88c9",
};

const hexColor = z.string().regex(
  /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/,
  "colors must be hex (#rgb, #rrggbb, or #rrggbbaa)",
);

/** A theme: display name + partial palette. Missing tokens fall back to defaults. */
export const Theme = z.object({
  name: z.string().trim().min(1).max(80),
  colors: z.partialRecord(z.enum(THEME_TOKENS), hexColor),
});
export type Theme = z.infer<typeof Theme>;

function themePath(dir?: string): string {
  return join(dir ?? boughHome(), "theme.json");
}

/** The stored theme, or null when none is set (default palette applies). */
export function loadTheme(dir?: string): Theme | null {
  const path = themePath(dir);
  if (!existsSync(path)) return null;
  try {
    const parsed = Theme.safeParse(JSON.parse(readFileSync(path, "utf8")));
    return parsed.success ? parsed.data : null; // corrupt file → default palette
  } catch {
    return null;
  }
}

export function saveTheme(theme: Theme, dir?: string): Theme {
  const path = themePath(dir);
  mkdirSync(join(path, ".."), { recursive: true });
  writeFileSync(path, JSON.stringify(theme, null, 2) + "\n");
  return theme;
}

/** Remove the stored theme — the UI falls back to the default palette. */
export function clearTheme(dir?: string): void {
  rmSync(themePath(dir), { force: true });
}
