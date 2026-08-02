/**
 * The vector layer, proven in a SUBPROCESS: `Database.setCustomSQLite` must run
 * before the first Database open in a process, and a shared `bun test` process
 * has long since opened one. The fixture prints one JSON line; this asserts on it.
 *
 * Skipped without an extension-capable SQLite or a local model file — the layer
 * is optional by design, and a test that needed the network to download a model
 * would not belong in `bun test`. On a machine with both (any dev machine that
 * has run the server once), it runs in full.
 */

import { test } from "bun:test";
import { deepStrictEqual } from "node:assert";
import { existsSync } from "node:fs";
import { mkdtemp, rm } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { join } from "node:path";

const MODEL = join(homedir(), ".bough/models/all-MiniLM-L6-v2.e4ce9877.q8_0.gguf");
const CAPABLE = process.platform === "darwin"
  ? existsSync("/opt/homebrew/opt/sqlite/lib/libsqlite3.dylib") ||
    existsSync("/usr/local/opt/sqlite/lib/libsqlite3.dylib")
  : true;

test.skipIf(!CAPABLE || !existsSync(MODEL))(
  "drain embeds recorded commands and similar() answers a fuzzy query",
  async () => {
    const dir = await mkdtemp(join(tmpdir(), "bough-embed-"));
    try {
      const r = Bun.spawnSync([
        process.execPath,
        new URL("./embed_fixture.ts", import.meta.url).pathname,
        dir,
        MODEL,
      ]);
      const line = r.stdout.toString().trim().split("\n").at(-1) ?? "";
      deepStrictEqual(JSON.parse(line), {
        drained: 4,
        top: "docker exec -it myapp-dev-1 bash",
      });
    } finally {
      await rm(dir, { recursive: true, force: true });
    }
  },
  30_000,
);
