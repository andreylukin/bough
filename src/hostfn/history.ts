/**
 * `history.sql(...)` — agent-driven recall over the command-history memory.
 *
 * The design choice this file embodies: the HARNESS does not decide what past
 * commands are relevant; the PROGRAM queries for them. Bough programs already
 * write JavaScript, so the recall surface is SQL over the command_history table
 * group (db/schema.sql), not a bespoke search DSL that would need its own docs.
 *
 * Read-only is enforced twice, both at the connection: the handle is opened
 * `{readonly: true}` AND `PRAGMA query_only = ON` (which also covers anything a
 * clever statement ATTACHes). The keyword check on top exists only to give a
 * write attempt a corrective message instead of a bare SQLITE_READONLY.
 *
 * `history.similar` is declared in the protocol but granted by the optional
 * vector layer (`history/embed.ts`); without it the verb rejects catchably,
 * pointing at the FTS path that always works.
 */

import { Database } from "bun:sqlite";
import { ProgramError } from "../errors.ts";
import { dbPath } from "../paths.ts";
import type { HostFns } from "../types.ts";

/** Bounded so one greedy SELECT cannot flood a round's tool result. */
const MAX_ROWS = 200;

/** Injected seams; tests point `path` at their own database file. */
export interface HistoryDeps {
  /** The database file to read. Defaults to the live `paths.dbPath()`. */
  path?: string;
  /**
   * Vector recall, wired by the optional embedding layer when it is available.
   * Absent = `history.similar` rejects with a pointer to `history.sql`.
   */
  similar?: (text: string) => Promise<unknown[]>;
}

/** The tables a program may read. Names only — the message the error shows. */
const SURFACE = "command_history, command_tags, command_dirs, command_history_fts";

function assertSelect(sql: string): void {
  const head = sql.replace(/^\s*(--[^\n]*\n|\/\*[\s\S]*?\*\/|\s)+/g, "").slice(0, 8)
    .toUpperCase();
  if (!head.startsWith("SELECT") && !head.startsWith("WITH")) {
    throw new ProgramError(
      `history.sql() is read-only: statements must start with SELECT or WITH. ` +
        `Queryable tables: ${SURFACE}.`,
    );
  }
}

/**
 * Wire `history` for one turn. The connection is opened lazily on first use and
 * kept for the life of the host-fn set (one turn), so a turn that never recalls
 * pays nothing.
 */
export function createHistoryHostFn(deps: HistoryDeps = {}): Pick<HostFns, "history"> {
  let db: Database | undefined;
  const open = (): Database => {
    if (db) return db;
    db = new Database(deps.path ?? dbPath(), { readonly: true });
    db.exec("PRAGMA query_only = ON");
    // A concurrent writer holding the rollback journal must surface as a brief
    // wait, not a spurious "database is locked" in the program.
    db.exec("PRAGMA busy_timeout = 2000");
    return db;
  };

  return {
    history: (verb: string, argsJson: string): Promise<string> => {
      if (verb === "sql") {
        const sql = JSON.parse(argsJson);
        if (typeof sql !== "string" || !sql.trim()) {
          throw new ProgramError(
            `history.sql(query) takes one SQL string, e.g. history.sql("SELECT cmd, ` +
              `exit_code FROM command_history JOIN command_tags ON command_id = id ` +
              `WHERE tag = 'psql' ORDER BY ts DESC LIMIT 10")`,
          );
        }
        assertSelect(sql);
        try {
          const rows = open().prepare(sql).all();
          const capped = (rows as unknown[]).slice(0, MAX_ROWS);
          return Promise.resolve(JSON.stringify(capped));
        } catch (err) {
          // The model wrote the SQL; the driver's message is what lets it fix it.
          throw new ProgramError(
            `history.sql() failed: ${err instanceof Error ? err.message : String(err)}. ` +
              `Queryable tables: ${SURFACE}.`,
          );
        }
      }
      if (verb === "similar") {
        if (!deps.similar) {
          throw new ProgramError(
            `history.similar() needs the local embedding layer, which is not ` +
              `available here. Use history.sql() with tag or FTS matching instead: ` +
              `SELECT cmd FROM command_history_fts WHERE command_history_fts MATCH 'docker'.`,
          );
        }
        const text = JSON.parse(argsJson);
        if (typeof text !== "string" || !text.trim()) {
          throw new ProgramError(`history.similar(text) takes one query string.`);
        }
        return deps.similar(text).then((rows) => JSON.stringify(rows.slice(0, MAX_ROWS)));
      }
      throw new ProgramError(`unknown history verb "${verb}" — use sql or similar.`);
    },
  };
}
