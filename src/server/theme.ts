/**
 * UI theme: a named partial palette over the web UI's semantic tokens, persisted
 * at ~/.bough/theme.json and served via GET/PUT/DELETE /theme. The web client
 * fetches it on boot and applies each color as a `--bough-<token>` CSS variable,
 * overriding the defaults baked into web/src/global.css `:root` — so a theme is
 * pure data, no rebuild. DEFAULTS below mirrors that :root block (kept in sync by
 * hand; it exists server-side so the /theme skill can ground drafts in the real
 * default palette without reading the web bundle).
 */
import { z } from "zod/v4";
import { join } from "node:path";
import { homedir } from "node:os";
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";

/** The semantic tokens the web UI reads (web/src/theme.ts). Fixed contract. */
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

/** The default (built-in) palette — mirror of web/src/global.css :root. */
export const THEME_DEFAULTS: Record<ThemeToken, string> = {
  bg: "#0e1013",
  panel: "#14161a",
  panel2: "#161a1f",
  panel3: "#191c21",
  panelInset: "#1f2329",
  canvas: "#111318",
  border: "#2b3038",
  border2: "#23272e",
  border3: "#1c2026",
  hairline: "#3a414c",
  text: "#e7e9ed",
  text2: "#c9cdd4",
  muted: "#9aa1ac",
  muted2: "#656c77",
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
  return join(dir ?? join(homedir(), ".bough"), "theme.json");
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
