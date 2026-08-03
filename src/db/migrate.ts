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
import type { Database } from "bun:sqlite";
import { readFileSync } from "node:fs";

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
  return readFileSync(new URL("./schema.sql", import.meta.url), "utf8");
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
export function migrate(db: Database): number {
  const found = userVersion(db);
  if (found > SCHEMA_VERSION) {
    throw new Error(
      `this database was written by a newer bough (schema v${found}, this build ` +
        `understands v${SCHEMA_VERSION}). Opening it would silently downgrade the ` +
        `data. Upgrade bough, or point BOUGH_DB at a different file.`,
    );
  }
  rebuildDayOneCommandHistory(db);
  addScheduleSessionId(db);
  addCommandMessageId(db);
  db.exec(schemaSql());
  if (found < SCHEMA_VERSION) setUserVersion(db, SCHEMA_VERSION);
  return found;
}

/**
 * The third sanctioned reshape: command_history gained `message_id` (2026-08) — the
 * supervisor message whose program ran the command, so a recalled command reaches
 * the round around it.
 *
 * The closed-table invariant this file opens with was opened DELIBERATELY here: it
 * says a later task that needs a column "stops and asks", and this one did. The rule
 * it leaves behind is the one its two siblings already follow — a reshape is a named
 * function with a PRAGMA guard, never an `ALTER` in a `catch`, and it explains in
 * prose why the column could not have been derived instead. Three of these is not yet
 * a ladder; a fourth without a paragraph like this one would be.
 *
 * ALTER rather than rebuild, unlike the day-one reshape below: the memory is worth
 * more now than it was with a handful of rows, and existing commands losing only
 * their link to a program is a far smaller loss than losing the commands.
 */
function addCommandMessageId(db: Database): void {
  const table = db
    .prepare(`SELECT 1 AS x FROM sqlite_master WHERE type = 'table' AND name = 'command_history'`)
    .get();
  if (!table) return;
  const cols = db.prepare(`PRAGMA table_info(command_history)`).all() as { name: string }[];
  if (cols.some((c) => c.name === "message_id")) return;
  db.exec(`ALTER TABLE command_history ADD COLUMN message_id TEXT`);
}

/**
 * The second sanctioned reshape: schedules gained `session_id` (2026-08) — the
 * conversation each firing reports back to. Unlike command_history below, a
 * schedule is a USER RECORD, not a cache, so this one ALTERs in place and keeps
 * every row (existing schedules get NULL: they report to nobody, which is the
 * pre-change behavior). Idempotent by PRAGMA check, exactly like its sibling;
 * no-op on a fresh file, where `schema.sql` creates the column directly.
 */
function addScheduleSessionId(db: Database): void {
  const table = db
    .prepare(`SELECT 1 AS x FROM sqlite_master WHERE type = 'table' AND name = 'schedules'`)
    .get();
  if (!table) return;
  const cols = db.prepare(`PRAGMA table_info(schedules)`).all() as { name: string }[];
  if (cols.some((c) => c.name === "session_id")) return;
  db.exec(`ALTER TABLE schedules ADD COLUMN session_id TEXT`);
}

/**
 * The first sanctioned reshape, and a deliberate exception to "no migration
 * ladder": command_history gained `output_head`/`spill_path` the day after it
 * shipped (2026-08), while exactly two installs held a handful of rows. A file
 * whose command_history predates the columns has its command-history GROUP
 * dropped and recreated empty by the schema exec that follows — the memory is
 * an accumulating cache, not a record, and losing a day of it beats carrying a
 * permanent ALTER ladder for tables nothing else references. Deleted rows'
 * embeddings become orphans the embed layer never returns (their rowids stay
 * absent from a rebuilt history). No-op on every database born after.
 */
function rebuildDayOneCommandHistory(db: Database): void {
  const table = db
    .prepare(`SELECT 1 AS x FROM sqlite_master WHERE type = 'table' AND name = 'command_history'`)
    .get();
  if (!table) return;
  const cols = db.prepare(`PRAGMA table_info(command_history)`).all() as { name: string }[];
  if (cols.some((c) => c.name === "output_head")) return;
  db.exec(`DROP TABLE IF EXISTS command_history_fts;
           DROP TABLE IF EXISTS command_dirs;
           DROP TABLE IF EXISTS command_tags;
           DROP TABLE IF EXISTS command_history;`);
}

/** The file's stamped schema generation; 0 for a database this never touched. */
export function userVersion(db: Database): number {
  // `bun:sqlite` returns null, not undefined, when a statement yields no row.
  const row = db.prepare(`PRAGMA user_version`).get() as
    | { user_version?: number }
    | null;
  return Number(row?.user_version ?? 0);
}

/**
 * `PRAGMA user_version` takes no bound parameter, so the value is interpolated —
 * guarded here because an interpolated non-integer would be a SQL injection in the
 * one place this module writes SQL by concatenation.
 */
function setUserVersion(db: Database, version: number): void {
  if (!Number.isSafeInteger(version) || version < 0) {
    throw new Error(`refusing to stamp a non-integer schema version: ${version}`);
  }
  db.exec(`PRAGMA user_version = ${version}`);
}
