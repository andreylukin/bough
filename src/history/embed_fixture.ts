/**
 * Subprocess fixture for embed.test.ts — and a hand-runnable proof of the whole
 * vector layer. A FRESH process is the point: `Database.setCustomSQLite` must
 * precede the first Database open, which a shared `bun test` process cannot
 * guarantee, so the test spawns this instead.
 *
 * Usage: bun src/history/embed_fixture.ts <workdir> [modelPath]
 * Prints one JSON line: {drained, top} where `top` is the best `similar()` hit
 * for a query no keyword search could match.
 */

import { join } from "node:path";
import { enableSqliteExtensions } from "../db/extensions.ts";
import { openDb } from "../db/db.ts";
import type { CommandRecord } from "../types.ts";
import { createEmbedLayer } from "./embed.ts";

const [workdir, modelPath] = process.argv.slice(2);
if (!workdir) throw new Error("usage: embed_fixture.ts <workdir> [modelPath]");

if (!enableSqliteExtensions()) {
  console.log(JSON.stringify({ error: "extensions unavailable" }));
  process.exit(0);
}

const boughDb = join(workdir, "bough.db");
const db = openDb(boughDb);
db.createSession({ id: "s1", parentId: null, title: "s1", kind: "root", createdAt: 1 });
const rec = (over: Partial<CommandRecord>): CommandRecord => ({
  sessionId: "s1",
  ts: 1_000,
  repo: "repo",
  cmd: "true",
  tags: "",
  tagList: [],
  dirs: [],
  exitCode: 0,
  durationMs: 1,
  source: "live",
  ...over,
});
db.recordCommand(rec({ cmd: "docker exec -it myapp-dev-1 bash", tags: "docker:exec" }));
db.recordCommand(rec({ cmd: "psql -f migrations/004.sql", tags: "psql:migrate" }));
db.recordCommand(rec({ cmd: "git push origin main", tags: "git:push" }));
db.recordCommand(rec({ cmd: "bun test src/tui", tags: "bun:test" }));
db.close();

const embed = createEmbedLayer({
  boughDb,
  embedDb: join(workdir, "embeddings.db"),
  ...(modelPath ? { modelPath } : {}),
});
if (!embed) {
  console.log(JSON.stringify({ error: "layer refused" }));
  process.exit(0);
}
const drained = await embed.drain();
const hits = await embed.similar("how do I get into the running container") as { cmd: string }[];
embed.close();
console.log(JSON.stringify({ drained, top: hits[0]?.cmd ?? null }));
