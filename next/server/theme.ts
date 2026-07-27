/**
 * Theming: a NAMED PARTIAL palette over a fixed semantic token set, persisted at
 * `~/.bough/theme.json` and served over `GET`/`PUT`/`DELETE /theme` (spec §16).
 *
 * THE INVARIANT THIS HOLDS: **a theme is pure data, and the SERVER owns the token
 * set.** Two halves, and both matter.
 *
 * *Pure data* means a theme never becomes code. There is no compile step, no
 * component that "is" a theme, and nothing here that a client has to be rebuilt to
 * pick up: the TUI fetches this document at boot and paints the tokens it consumes as
 * truecolor (`tui/theme.ts`). That is what lets the picker preview a palette live on
 * cursor move and revert it on exit — browsing is a repaint, not a deploy.
 *
 * *The server owns the token set* is why `THEME_TOKENS` is here rather than in the
 * frozen wire schema. `schema/requests.ts` deliberately types `colors` as an open
 * `Record<string, string>` and says so: a Zod enum there would answer an unknown token
 * with a generic "invalid enum value" from the router's catch-all, while the thing the
 * author needs to be told is *which* token they misspelled and what the real ones are.
 * `validateTheme` below does exactly that, and it is the only gate — nothing else in
 * the tree may accept a palette.
 *
 * WHY THE THEME IS PARTIAL, AND WHY THE DEFAULTS ARE SERVED ALONGSIDE IT. A stored
 * theme names only the tokens it changes; everything else falls through to
 * `THEME_DEFAULTS`. So `GET /theme` answers `{theme, defaults}` rather than one merged
 * map: a client that only ever saw the merge could not tell a token the user *chose*
 * from one it inherited, and "reset this token" would be indistinguishable from
 * "set it to the value it already has". The merge is the client's, one line, and it is
 * `resolveColors` in `tui/theme.ts`.
 *
 * A CORRUPT FILE IS THE DEFAULT PALETTE, NOT AN ERROR. `loadTheme` answers `null` for
 * anything it cannot parse or validate. A hand-edited `theme.json` with a trailing
 * comma must not take the theme endpoint — or, since the TUI fetches it at boot, the
 * whole UI's colour — down with it. Writing is where validation bites; reading is
 * where it forgives.
 *
 * Ported from `src/server/theme.ts` (tokens, defaults, persistence). New here: the
 * three handlers, the token-naming validation, and `{theme, defaults}` as the served
 * shape — the old tree merged server-side.
 */
import { existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";
import { BadRequestError } from "../errors.ts";
import { themePath } from "../paths.ts";
import { PutThemeBody } from "../schema/requests.ts";
import { json, parseBody } from "./http.ts";

// ---------------------------------------------------------------------------
// The token contract
// ---------------------------------------------------------------------------

/**
 * The semantic tokens a theme may set. A FIXED contract, and deliberately wider than
 * what the TUI reads today: this is what `PUT /theme` validates against and what a
 * theme author is shown when they miss. Adding a token is a compatible change (old
 * themes simply do not set it); removing one is not.
 */
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

const TOKEN_SET: ReadonlySet<string> = new Set<string>(THEME_TOKENS);

/**
 * The built-in palette — the floor every partial theme falls through to.
 *
 * The contrast notes are carried over from the old tree because they are the reason
 * these particular hexes are here. Borders sit at least 3:1 against `bg` (`hairline`
 * higher still — it outlines the panels); `muted2` is TEXT rather than decoration and
 * clears WCAG AA at 4.91:1, which the earlier `#656c77` did not. `tui/theme.ts`'s
 * `FALLBACK` mirrors the subset the TUI consumes for the case where the server cannot
 * be reached — the two must not drift, and this is the one that wins whenever the
 * server is up.
 */
export const THEME_DEFAULTS: Record<ThemeToken, string> = {
  bg: "#0e1013",
  panel: "#14161a",
  panel2: "#161a1f",
  panel3: "#191c21",
  panelInset: "#1f2329",
  canvas: "#111318",
  border: "#5a616c",
  border2: "#484e57",
  border3: "#3c4149",
  hairline: "#666d79",
  text: "#e7e9ed",
  text2: "#c9cdd4",
  muted: "#9aa1ac",
  muted2: "#7a828e",
  green: "#4ec98f",
  amber: "#d9b45f",
  red: "#e2776e",
  blue: "#5c88c9",
};

/** `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa`. The TUI renders truecolor; nothing else. */
const HEX = /^#(?:[0-9a-fA-F]{3}|[0-9a-fA-F]{4}|[0-9a-fA-F]{6}|[0-9a-fA-F]{8})$/;

/** A theme: a display name and the tokens it overrides. Everything else inherits. */
export interface Theme {
  name: string;
  colors: Partial<Record<ThemeToken, string>>;
}

/** What `GET /theme` serves. The client merges; see the header for why. */
export interface ThemeState {
  theme: Theme | null;
  defaults: Record<ThemeToken, string>;
}

// ---------------------------------------------------------------------------
// Validation (pure)
// ---------------------------------------------------------------------------

/**
 * Validate a candidate theme, NAMING what is wrong.
 *
 * Pure, and separated from both the route and the file so the error text is directly
 * testable. Every message follows the rule spec §6 states for host functions and that
 * an API owes just as much: say what failed, the state that caused it, and the move
 * that resolves it. "invalid enum value" satisfies none of the three.
 *
 * Unknown tokens are collected rather than reported one at a time — a hand-written
 * palette usually misspells a family of them at once, and three round-trips to learn
 * three names is three times the work.
 */
export function validateTheme(input: { name: string; colors: Record<string, string> }): Theme {
  const name = input.name.trim();
  if (!name) throw new BadRequestError("theme name is required");

  const unknown = Object.keys(input.colors).filter((k) => !TOKEN_SET.has(k));
  if (unknown.length > 0) {
    throw new BadRequestError(
      `unknown theme token(s): ${unknown.join(", ")} — the token set is fixed: ` +
        THEME_TOKENS.join(", "),
    );
  }

  const bad = Object.entries(input.colors).filter(([, v]) => !HEX.test(v));
  if (bad.length > 0) {
    throw new BadRequestError(
      `colors must be hex (#rgb, #rrggbb or #rrggbbaa): ` +
        bad.map(([k, v]) => `${k}=${JSON.stringify(v)}`).join(", "),
    );
  }

  // Rebuilt rather than passed through, so what is persisted contains exactly the
  // validated keys and nothing a looser parse let ride along.
  const colors: Partial<Record<ThemeToken, string>> = {};
  for (const [k, v] of Object.entries(input.colors)) colors[k as ThemeToken] = v;
  return { name, colors };
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

/**
 * The stored theme, or `null` when none is set — which is the ordinary state, not a
 * failure. A file that cannot be read, parsed or validated is also `null`: see the
 * header. `path` is injected by tests so nothing here touches a real `~/.bough`.
 */
export function loadTheme(path: string = themePath()): Theme | null {
  try {
    if (!existsSync(path)) return null;
    const raw: unknown = JSON.parse(readFileSync(path, "utf8"));
    if (typeof raw !== "object" || raw === null) return null;
    const { name, colors } = raw as { name?: unknown; colors?: unknown };
    if (typeof name !== "string") return null;
    const table = typeof colors === "object" && colors !== null
      ? (colors as Record<string, unknown>)
      : {};
    const clean: Partial<Record<ThemeToken, string>> = {};
    for (const [k, v] of Object.entries(table)) {
      // Forgiving on READ: an unknown token or a bad hex in a hand-edited file is
      // dropped rather than discarding the whole palette with it.
      if (TOKEN_SET.has(k) && typeof v === "string" && HEX.test(v)) clean[k as ThemeToken] = v;
    }
    const trimmed = name.trim();
    return trimmed ? { name: trimmed, colors: clean } : null;
  } catch {
    return null;
  }
}

/** Persist a validated theme. Creates `~/.bough` if this is the first write. */
export function saveTheme(theme: Theme, path: string = themePath()): Theme {
  mkdirSync(dirname(path), { recursive: true });
  writeFileSync(path, JSON.stringify(theme, null, 2) + "\n");
  return theme;
}

/** Remove the stored theme; the palette falls back to `THEME_DEFAULTS`. */
export function clearTheme(path: string = themePath()): void {
  rmSync(path, { force: true });
}

/** The served document for whatever is (or is not) stored. */
export function themeState(path: string = themePath()): ThemeState {
  return { theme: loadTheme(path), defaults: THEME_DEFAULTS };
}

// ---------------------------------------------------------------------------
// REST
// ---------------------------------------------------------------------------

/**
 * `GET /theme` — `{theme, defaults}`.
 *
 * `function` DECLARATIONS rather than `const` arrows, for the same reason the history
 * handlers are: `server/app.ts` reads these bindings while it builds its route table
 * at module scope, and a hoisted declaration exists from module instantiation whereas
 * a `const` is in its temporal dead zone if this module happens to evaluate first.
 *
 * Always 200. "No theme is set" is an ANSWER — it is the default palette — and a 404
 * would make every client branch on a condition that is the normal case.
 */
export function getThemeH(): Response {
  return json(themeState());
}

/** `PUT /theme` — adopt a named partial palette. 200 with the new state. */
export async function putThemeH(req: Request): Promise<Response> {
  const body = await parseBody(req, PutThemeBody);
  const theme = validateTheme(body);
  saveTheme(theme);
  return json({ theme, defaults: THEME_DEFAULTS });
}

/**
 * `DELETE /theme` — back to the built-in palette. Idempotent: deleting a theme that
 * was never set is a success, because the state the caller asked for is the state
 * they get.
 */
export function deleteThemeH(): Response {
  clearTheme();
  return json({ theme: null, defaults: THEME_DEFAULTS });
}
