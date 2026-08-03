/**
 * `history.sql()` — the read-only recall surface. Runs against a real database
 * FILE (not `:memory:`) because the host fn opens its own readonly connection to
 * the same path the writer used; two `:memory:` handles would be two databases.
 */

import { test } from "bun:test";
import { deepStrictEqual, ok } from "node:assert";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { openDb } from "../db/db.ts";
import { ProgramError } from "../errors.ts";
import type { CommandRecord } from "../types.ts";
import { createHistoryHostFn } from "./history.ts";

function eq(actual: unknown, expected: unknown, message?: string): void {
  deepStrictEqual(actual, expected, message);
}

async function rejectsWith(fn: () => Promise<unknown>): Promise<Error> {
  try {
    await fn();
  } catch (err) {
    ok(err instanceof ProgramError, `expected ProgramError, got ${err}`);
    return err;
  }
  throw new Error("expected a rejection");
}

function record(over: Partial<CommandRecord>): CommandRecord {
  return {
    sessionId: "s1",
    ts: 1_000,
    repo: "repo",
    cmd: "true",
    tags: "",
    tagList: [],
    dirs: [],
    exitCode: 0,
    durationMs: 1,
    outputHead: "",
    spillPath: null,
    source: "live",
    ...over,
  };
}

async function withDb(
  fn: (path: string) => Promise<void>,
  rows: CommandRecord[] = [],
): Promise<void> {
  const dir = await mkdtemp(join(tmpdir(), "bough-histfn-"));
  const path = join(dir, "h.db");
  try {
    const db = openDb(path);
    db.createSession({ id: "s1", parentId: null, title: "s1", kind: "root", createdAt: 1 });
    for (const r of rows) db.recordCommand(r);
    db.close();
    await fn(path);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
}

test("history.sql selects over the memory, tags and FTS included", () =>
  withDb(async (path) => {
    const { history } = createHistoryHostFn({ path });
    const byTag = JSON.parse(
      await history!(
        "sql",
        JSON.stringify(
          `SELECT cmd FROM command_history JOIN command_tags t ON t.command_id = id
            WHERE t.tag = 'docker' AND exit_code = 0 ORDER BY ts`,
        ),
      ),
    );
    eq(byTag, [{ cmd: "docker exec -it app bash" }]);
    const byFts = JSON.parse(
      await history!(
        "sql",
        JSON.stringify(
          `SELECT h.cmd FROM command_history_fts f JOIN command_history h ON h.id = f.command_id
            WHERE command_history_fts MATCH 'migrate'`,
        ),
      ),
    );
    eq(byFts, [{ cmd: "psql -f migrations/004.sql" }]);
  }, [
    record({ cmd: "docker exec -it app bash", tags: "docker:exec", tagList: ["docker", "exec"] }),
    record({ cmd: "docker rm app", tags: "docker", tagList: ["docker"], exitCode: 1, ts: 2_000 }),
    record({ cmd: "psql -f migrations/004.sql", tags: "psql:migrate", tagList: ["psql", "migrate"] }),
  ]));

test("history.sql refuses writes with a corrective message, twice over", () =>
  withDb(async (path) => {
    const { history } = createHistoryHostFn({ path });
    // The keyword gate names the contract…
    const err = await rejectsWith(() =>
      history!("sql", JSON.stringify("DELETE FROM command_history"))
    );
    ok(err.message.includes("read-only"), err.message);
    // …and a SELECT-shaped statement that still tries to write hits the
    // connection-level guard (query_only), not the data.
    const sneaky = await rejectsWith(() =>
      history!("sql", JSON.stringify("WITH x AS (SELECT 1) INSERT INTO command_tags SELECT 1, 'a'"))
    );
    ok(sneaky.message.includes("history.sql() failed"), sneaky.message);
  }));

test("history.sql surfaces the driver's message so the model can fix its query", () =>
  withDb(async (path) => {
    const { history } = createHistoryHostFn({ path });
    const err = await rejectsWith(() =>
      history!("sql", JSON.stringify("SELECT nope FROM command_history"))
    );
    ok(err.message.includes("no such column"), err.message);
  }));

test("history.sql caps the rows one call can flood a round with", () =>
  withDb(async (path) => {
    const { history } = createHistoryHostFn({ path });
    const rows = JSON.parse(
      await history!("sql", JSON.stringify("SELECT ts FROM command_history")),
    );
    eq(rows.length, 200);
  }, Array.from({ length: 205 }, (_, i) => record({ ts: i }))));

test("history.similar without the vector layer points at the path that works", () =>
  withDb(async (path) => {
    const { history } = createHistoryHostFn({ path });
    const err = await rejectsWith(() => history!("similar", JSON.stringify("docker stuff")));
    ok(err.message.includes("history.sql()"), err.message);
  }));

test("history.similar delegates to the injected layer when present", () =>
  withDb(async (path) => {
    const { history } = createHistoryHostFn({
      path,
      similar: (text) => Promise.resolve([{ cmd: `matched:${text}` }]),
    });
    const rows = JSON.parse(await history!("similar", JSON.stringify("get into the container")));
    eq(rows, [{ cmd: "matched:get into the container" }]);
  }));

test("an unknown verb names the two that exist", () =>
  withDb(async (path) => {
    const { history } = createHistoryHostFn({ path });
    const err = await rejectsWith(() => history!("query", JSON.stringify("SELECT 1")));
    ok(err.message.includes("sql or similar"), err.message);
  }));

test("FTS matches what a command PRINTED, and the row points at its spill file", () =>
  withDb(async (path) => {
    const { history } = createHistoryHostFn({ path });
    const rows = JSON.parse(
      await history!(
        "sql",
        JSON.stringify(
          `SELECT h.cmd, h.spill_path FROM command_history_fts f
            JOIN command_history h ON h.id = f.command_id
           WHERE command_history_fts MATCH 'output_head:relation'`,
        ),
      ),
    );
    eq(rows, [{ cmd: "psql -f 004.sql", spill_path: "/scratch/s1/psql.txt" }]);
  }, [
    record({
      cmd: "psql -f 004.sql",
      outputHead: 'ERROR: relation "users" does not exist',
      spillPath: "/scratch/s1/psql.txt",
    }),
    record({ cmd: "true", outputHead: "ok" }),
  ]));
