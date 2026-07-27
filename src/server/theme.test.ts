/**
 * Tests for theming.
 *
 * Three things are pinned here, and only the first is obvious.
 *
 * 1. The token contract: an unknown token is rejected with a message that NAMES it and
 *    lists the real ones, because "invalid enum value" from the router's catch-all is
 *    what the frozen `PutThemeBody` deliberately does not do (`schema/requests.ts`
 *    types `colors` open and says the theme module owns validation).
 * 2. Partial-ness survives the round trip. A theme names only what it changes and the
 *    served document keeps `theme` and `defaults` SEPARATE, so a client can tell a
 *    chosen colour from an inherited one. A merged answer would look identical and be
 *    unable to express "reset this token".
 * 3. Reading forgives what writing rejects. A hand-edited `theme.json` — trailing
 *    junk, an unknown token, a bad hex — resolves to a usable palette or to the
 *    default, never to an error: the TUI fetches this at boot, so an unparseable file
 *    must not be able to take the UI's colour down with it.
 *
 * Everything runs through `createHandler(ctx)` with no socket bound, over a
 * `BOUGH_HOME` pointed at a temp directory — never a real `~/.bough`. Assertions come
 * from `node:assert/strict`: jsr.io is unreachable here and a test that cannot run
 * offline does not belong in `deno task test`.
 */
import assert from "node:assert/strict";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { Bus } from "../bus.ts";
import type { AppCtx, Db } from "../types.ts";
import { createHandler, type Route, route } from "./app.ts";
import {
  clearTheme,
  deleteThemeH,
  getThemeH,
  loadTheme,
  putThemeH,
  saveTheme,
  THEME_DEFAULTS,
  THEME_TOKENS,
  type ThemeState,
  validateTheme,
} from "./theme.ts";

// ---------------------------------------------------------------------------
// Harness
// ---------------------------------------------------------------------------

const ROUTES: Route[] = [
  route("GET", "/theme", getThemeH),
  route("PUT", "/theme", putThemeH),
  route("DELETE", "/theme", deleteThemeH),
];

/** A fabricated ctx: the theme surface reads no database and no LLM. */
function fakeCtx(): AppCtx {
  return { db: {} as Db, bus: new Bus() };
}

/** Point `BOUGH_HOME` at a fresh temp dir for the body of `fn`, then restore it. */
async function withHome(fn: (home: string) => Promise<void> | void): Promise<void> {
  const previous = Deno.env.get("BOUGH_HOME");
  const home = mkdtempSync(join(tmpdir(), "bough-theme-"));
  Deno.env.set("BOUGH_HOME", home);
  try {
    await fn(home);
  } finally {
    if (previous === undefined) Deno.env.delete("BOUGH_HOME");
    else Deno.env.set("BOUGH_HOME", previous);
  }
}

async function request<T>(
  method: string,
  body?: unknown,
): Promise<{ status: number; body: T }> {
  const handler = createHandler(fakeCtx(), { routes: ROUTES });
  const res = await handler(
    new Request("http://localhost/theme", {
      method,
      ...(body === undefined ? {} : {
        body: JSON.stringify(body),
        headers: { "content-type": "application/json" },
      }),
    }),
  );
  return { status: res.status, body: (await res.json()) as T };
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

Deno.test("an unknown token is rejected with a message naming it and the real tokens", () => {
  let err!: Error & { status?: number };
  assert.throws(
    () => validateTheme({ name: "Typo", colors: { accent: "#ffffff", forground: "#000000" } }),
    (e: Error & { status?: number }) => {
      err = e;
      return true;
    },
  );
  assert.equal(err.status, 400);
  // Both offenders in one answer: a palette usually misspells a family of tokens at
  // once, and three round-trips to learn three names is three times the work.
  assert.match(err.message, /accent/);
  assert.match(err.message, /forground/);
  // …and the real set, so the fix does not need a docs lookup.
  assert.match(err.message, /green/);
  assert.match(err.message, /panelInset/);
});

Deno.test("a non-hex colour is rejected, naming the token and the value", () => {
  let err!: Error & { status?: number };
  assert.throws(
    () => validateTheme({ name: "Bad", colors: { green: "rebeccapurple" } }),
    (e: Error & { status?: number }) => {
      err = e;
      return true;
    },
  );
  assert.equal(err.status, 400);
  assert.match(err.message, /green/);
  assert.match(err.message, /rebeccapurple/);
});

Deno.test("every hex length the TUI can paint is accepted, and the name is trimmed", () => {
  const theme = validateTheme({
    name: "  Spaced  ",
    colors: { green: "#abc", amber: "#abcd", red: "#aabbcc", blue: "#aabbccdd" },
  });
  assert.equal(theme.name, "Spaced");
  assert.deepEqual(Object.keys(theme.colors).sort(), ["amber", "blue", "green", "red"]);
});

Deno.test("an empty name is rejected", () => {
  assert.throws(() => validateTheme({ name: "   ", colors: {} }));
});

Deno.test("THEME_DEFAULTS covers every token, and every default is hex", () => {
  // The defaults are the floor a partial theme falls through to; a missing one would
  // paint a token as terminal-default grey with nothing to notice it by.
  for (const token of THEME_TOKENS) {
    const value = THEME_DEFAULTS[token];
    assert.equal(typeof value, "string", `${token} has no default`);
    assert.match(value, /^#[0-9a-fA-F]{3,8}$/, `${token} default is not hex`);
  }
  assert.equal(Object.keys(THEME_DEFAULTS).length, THEME_TOKENS.length);
});

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

Deno.test("a corrupt theme file reads as the default palette, not an error", async () => {
  await withHome((home) => {
    const path = join(home, "theme.json");
    writeFileSync(path, "{ not json,");
    assert.equal(loadTheme(path), null);
  });
});

Deno.test("a hand-edited file keeps its valid tokens and drops the rest", async () => {
  await withHome((home) => {
    const path = join(home, "theme.json");
    writeFileSync(
      path,
      JSON.stringify({ name: "Hand", colors: { green: "#123456", nope: "#111111", red: "zzz" } }),
    );
    // Forgiving on READ, strict on WRITE: the palette survives, the junk does not.
    assert.deepEqual(loadTheme(path), { name: "Hand", colors: { green: "#123456" } });
  });
});

Deno.test("clearTheme on a theme that was never set is a success", async () => {
  await withHome((home) => {
    const path = join(home, "theme.json");
    clearTheme(path); // no file yet
    clearTheme(path); // still none
    assert.equal(loadTheme(path), null);
  });
});

Deno.test("saveTheme creates the data root on first write", async () => {
  await withHome((home) => {
    const path = join(home, "nested", "theme.json");
    saveTheme({ name: "Fjord", colors: { green: "#5c88c9" } }, path);
    assert.deepEqual(JSON.parse(readFileSync(path, "utf8")), {
      name: "Fjord",
      colors: { green: "#5c88c9" },
    });
  });
});

// ---------------------------------------------------------------------------
// The routes
// ---------------------------------------------------------------------------

Deno.test("GET /theme with nothing stored is 200 with the defaults — not a 404", async () => {
  await withHome(async () => {
    const res = await request<ThemeState>("GET");
    assert.equal(res.status, 200);
    assert.equal(res.body.theme, null);
    assert.equal(res.body.defaults.green, THEME_DEFAULTS.green);
  });
});

Deno.test("PUT then GET round-trips a PARTIAL palette with defaults kept separate", async () => {
  await withHome(async () => {
    const put = await request<ThemeState>("PUT", { name: "Iris", colors: { green: "#9a7fd1" } });
    assert.equal(put.status, 200);
    assert.deepEqual(put.body.theme, { name: "Iris", colors: { green: "#9a7fd1" } });

    const get = await request<ThemeState>("GET");
    // The whole point of serving both halves: the client can still tell that `amber`
    // is INHERITED rather than chosen, which a merged map cannot express. Asserted
    // BEFORE the deepEqual, which narrows `colors` to the literal it matched.
    assert.equal(get.body.theme?.colors.amber, undefined);
    assert.equal(get.body.defaults.amber, THEME_DEFAULTS.amber);
    assert.deepEqual(get.body.theme, { name: "Iris", colors: { green: "#9a7fd1" } });
  });
});

Deno.test("PUT with an unknown token is a 400 naming it, and does not overwrite", async () => {
  await withHome(async () => {
    await request("PUT", { name: "Iris", colors: { green: "#9a7fd1" } });
    const bad = await request<{ error: string }>("PUT", {
      name: "Broken",
      colors: { forground: "#000000" },
    });
    assert.equal(bad.status, 400);
    assert.match(bad.body.error, /forground/);
    // The stored theme is untouched: validation happens before the write.
    const get = await request<ThemeState>("GET");
    assert.equal(get.body.theme?.name, "Iris");
  });
});

Deno.test("DELETE /theme returns to the built-in palette and is idempotent", async () => {
  await withHome(async () => {
    await request("PUT", { name: "Iris", colors: { green: "#9a7fd1" } });
    const first = await request<ThemeState>("DELETE");
    assert.equal(first.status, 200);
    assert.equal(first.body.theme, null);
    // Idempotent: the state the caller asked for is the state they get.
    const second = await request<ThemeState>("DELETE");
    assert.equal(second.status, 200);
    assert.equal(second.body.theme, null);
    assert.equal((await request<ThemeState>("GET")).body.theme, null);
  });
});
