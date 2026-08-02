/**
 * SQLite loadable-extension capability, decided once per process.
 *
 * Bun's `Database.setCustomSQLite` must be called BEFORE the first `Database` is
 * opened, exactly once — after that the choice is frozen. On macOS it is the only
 * way to get extension loading at all (Apple's system SQLite compiles it out), so
 * the server entry calls `enableSqliteExtensions()` as its first act, before
 * anything opens bough.db. On Linux, Bun's bundled SQLite loads extensions as-is
 * and no swap is needed.
 *
 * Everything is graceful-absence: no Homebrew SQLite (or `BOUGH_NO_EMBED=1`) just
 * means `extensionsEnabled()` is false and the optional vector layer never
 * exists; tags + FTS recall are unaffected. Nothing here throws.
 */

import { Database } from "bun:sqlite";
import { existsSync } from "node:fs";

/** Homebrew's SQLite, the usual macOS source of an extension-capable build. */
const DARWIN_SQLITE_PATHS = [
  "/opt/homebrew/opt/sqlite/lib/libsqlite3.dylib",
  "/usr/local/opt/sqlite/lib/libsqlite3.dylib",
];

let decided: boolean | undefined;

/**
 * Try to make this process capable of `loadExtension`. Idempotent; the first
 * call decides. Returns whether extension loading is expected to work.
 */
export function enableSqliteExtensions(): boolean {
  if (decided !== undefined) return decided;
  if (process.env.BOUGH_NO_EMBED) return (decided = false);
  if (process.platform !== "darwin") return (decided = true);
  const lib = DARWIN_SQLITE_PATHS.find((p) => existsSync(p));
  if (!lib) return (decided = false);
  try {
    Database.setCustomSQLite(lib);
    return (decided = true);
  } catch {
    // Something already opened a Database — the swap window has passed. The
    // vector layer stays off for this process; nothing else changes.
    return (decided = false);
  }
}

/** Whether `enableSqliteExtensions()` ran and succeeded. Never triggers the swap. */
export function extensionsEnabled(): boolean {
  return decided === true;
}
