/**
 * Bringing a database file up to the frozen schema, once per open.
 *
 * The invariant: **migration is forward-only and idempotent.** Applying it to a
 * fresh file and applying it to a file that already has every table must leave the
 * same database and must never fail. That is why `schema.sql` is one block of
 * `CREATE ... IF NOT EXISTS` statements rather than a ladder of numbered steps: the
 * table set is closed (plan §4), so there is nothing to step *through*. A later task
 * that needs a column stops and asks instead of appending an `ALTER TABLE` here.
 *
 * The old tree learned this the hard way. It carried a `#migrate()` that ran twenty
 * `ALTER TABLE ... ADD COLUMN` statements inside a bare `catch {}` swallowing the
 * duplicate-column error, and the consequence was a schema you could only discover
 * by reading the migration history in order. Nothing in this file swallows an error.
 *
 * `user_version` is the forward-only guard, and it is the ONLY thing here that
 * resembles a version ladder. It exists so a database written by a future bough is
 * refused loudly at open rather than silently half-read: opening downgrades data,
 * and a corrupted-looking read hours later is a much worse failure than a startup
 * error naming both versions.
 *
 * `schema.sql` is read from disk at open rather than inlined as a string, so the
 * file stays the single readable definition of the schema and cannot drift from what
 * is actually applied.
 */
import type { DatabaseSync } from "node:sqlite";

/**
 * The schema generation this build writes and understands.
 *
 * Bumping it is a deliberate act that belongs with a change to `schema.sql`, and
 * since the table set is closed there is currently no reason to. It is not a
 * migration step counter — there are no steps.
 */
export const SCHEMA_VERSION = 1;

/** The frozen schema text, read from `db/schema.sql` beside this module. */
export function schemaSql(): string {
  return Deno.readTextFileSync(new URL("./schema.sql", import.meta.url));
}

/**
 * Apply the frozen schema to `db` and stamp its version.
 *
 * Safe to call on every open: on a fresh file it creates everything, and on an
 * existing one every statement is a no-op. Returns the `user_version` the file was
 * at *before* this call, so a caller can tell a first open (0) from a reopen.
 *
 * Throws when the file was written by a newer bough than this one — forward-only
 * means exactly that, and there is no downgrade path.
 */
export function migrate(db: DatabaseSync): number {
  const found = userVersion(db);
  if (found > SCHEMA_VERSION) {
    throw new Error(
      `this database was written by a newer bough (schema v${found}, this build ` +
        `understands v${SCHEMA_VERSION}). Opening it would silently downgrade the ` +
        `data. Upgrade bough, or point BOUGH_DB at a different file.`,
    );
  }
  db.exec(schemaSql());
  if (found < SCHEMA_VERSION) setUserVersion(db, SCHEMA_VERSION);
  return found;
}

/** The file's stamped schema generation; 0 for a database this never touched. */
export function userVersion(db: DatabaseSync): number {
  const row = db.prepare(`PRAGMA user_version`).get() as
    | { user_version?: number }
    | undefined;
  return Number(row?.user_version ?? 0);
}

/**
 * `PRAGMA user_version` takes no bound parameter, so the value is interpolated —
 * guarded here because an interpolated non-integer would be a SQL injection in the
 * one place this module writes SQL by concatenation.
 */
function setUserVersion(db: DatabaseSync, version: number): void {
  if (!Number.isSafeInteger(version) || version < 0) {
    throw new Error(`refusing to stamp a non-integer schema version: ${version}`);
  }
  db.exec(`PRAGMA user_version = ${version}`);
}
