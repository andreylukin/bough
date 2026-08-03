/**
 * The optional vector layer over the command-history memory: local embeddings,
 * generated INSIDE SQLite.
 *
 * Two loadable extensions do all the work — `sqlite-vec` (the `vec0` KNN table)
 * and `sqlite-lembed` (GGUF embedding models as a SQL function) — so there is no
 * Node native module, no subprocess, and no API call anywhere in this file. The
 * whole layer is one SQL statement per direction:
 *
 *   drain:    INSERT INTO vec_index SELECT id, lembed('embed', tags || cmd) …
 *   similar:  … WHERE embedding MATCH lembed('embed', ?) ORDER BY distance
 *
 * Vectors live in their OWN database file (`~/.bough/embeddings.db`), never in
 * bough.db: every other connection in the system (the migrator, `bough tags sql`'s
 * readonly handle, the drain's own reader) lacks the
 * vec0 module, and a virtual table they cannot even parse must not sit in a file
 * they walk. The embed connection ATTACHes bough.db and treats it as read-only
 * by discipline; embeddings.db is fully derived state and can be deleted freely.
 *
 * Everything is graceful-absence (`create()` returns null, `drain()` returns 0)
 * except `similar()`, whose failure is a catchable, explanatory rejection — it
 * is only reachable when the layer exists, and the model deserves to know why a
 * recall failed. Embedding is CPU-bound and synchronous inside SQLite, so the
 * drain batch is kept small enough (~64) to never block the event loop long.
 */

import { Database } from "bun:sqlite";
import { existsSync, mkdirSync, renameSync } from "node:fs";
import { basename, dirname } from "node:path";
import { getLoadablePath as lembedPath } from "sqlite-lembed";
import { getLoadablePath as vecPath } from "sqlite-vec";
import { extensionsEnabled } from "../db/extensions.ts";
import { boughPath, dbPath } from "../paths.ts";

/** ~25MB, 384 dims — plenty for a corpus of thousands of short command docs. */
const MODEL_URL =
  "https://huggingface.co/asg017/sqlite-lembed-model-examples/resolve/main/all-MiniLM-L6-v2/all-MiniLM-L6-v2.e4ce9877.q8_0.gguf";
const DEFAULT_MODEL_FILE = "all-MiniLM-L6-v2.e4ce9877.q8_0.gguf";

/** Small enough that one drain never blocks the event loop noticeably. */
const DRAIN_BATCH = 64;
const KNN_LIMIT = 10;
/** Command text beyond this adds latency, not meaning, to an embedding. */
const DOC_CMD_CHARS = 500;

export interface EmbedOptions {
  /** The command-history database to ATTACH. Defaults to the live `dbPath()`. */
  boughDb?: string;
  /** Where vectors live. Defaults to `~/.bough/embeddings.db`. */
  embedDb?: string;
  /** A GGUF embedding model. Defaults to `BOUGH_EMBED_MODEL`, else the bundled-by-download MiniLM. */
  modelPath?: string;
}

export interface EmbedLayer {
  /** Embed pending commands, up to one batch. Resolves to how many were embedded. */
  drain(): Promise<number>;
  /** KNN over the memory. Rejects with a plain, explanatory Error on any failure. */
  similar(text: string): Promise<unknown[]>;
  close(): void;
}

/**
 * Build the layer, or null when this process cannot host it (no extension
 * support, or `BOUGH_NO_EMBED`). Null is the everyday macOS-without-Homebrew
 * answer and callers treat it as "the feature does not exist".
 */
export function createEmbedLayer(opts: EmbedOptions = {}): EmbedLayer | null {
  if (!extensionsEnabled()) return null;
  const boughDb = opts.boughDb ?? dbPath();
  const embedDb = opts.embedDb ?? boughPath("embeddings.db");
  const modelPath = opts.modelPath ?? process.env.BOUGH_EMBED_MODEL ??
    boughPath("models", DEFAULT_MODEL_FILE);

  let db: Database | undefined;
  /** The one-time init, memoized including its failure. */
  let init: Promise<Database> | undefined;

  const ensureModel = async (): Promise<void> => {
    if (existsSync(modelPath)) return;
    if (process.env.BOUGH_EMBED_MODEL || opts.modelPath) {
      throw new Error(`embedding model not found at ${modelPath}`);
    }
    mkdirSync(dirname(modelPath), { recursive: true });
    const res = await fetch(MODEL_URL);
    if (!res.ok) throw new Error(`model download failed: HTTP ${res.status} for ${MODEL_URL}`);
    // Download to a sibling temp name and rename, so a killed download can never
    // leave a half-written file that every later boot trusts.
    const tmp = `${modelPath}.download`;
    await Bun.write(tmp, await res.arrayBuffer());
    renameSync(tmp, modelPath);
  };

  const open = async (): Promise<Database> => {
    await ensureModel();
    mkdirSync(dirname(embedDb), { recursive: true });
    const d = new Database(embedDb);
    d.loadExtension(vecPath());
    d.loadExtension(lembedPath());
    d.run(`ATTACH DATABASE ? AS src`, [boughDb]);
    d.run(
      `INSERT INTO temp.lembed_models(name, model)
        SELECT 'embed', lembed_model_from_file(?)`,
      [modelPath],
    );
    // The model decides the dimension; probe it rather than hardcode, so an
    // env-supplied model of any width just works.
    const dims =
      (d.query(`SELECT length(lembed('embed', 'probe')) / 4 AS d`).get() as { d: number }).d;
    d.run(`CREATE TABLE IF NOT EXISTS embed_meta (key TEXT PRIMARY KEY, value TEXT)`);
    const meta = d.query(`SELECT value FROM embed_meta WHERE key = 'model'`).get() as
      | { value: string }
      | null;
    const modelId = `${basename(modelPath)}:${dims}`;
    if (meta?.value !== modelId) {
      // A different model's vectors are not comparable to this one's. The store
      // is fully derived, so the honest move is a rebuild from zero.
      d.run(`DROP TABLE IF EXISTS vec_index`);
      d.run(
        `INSERT INTO embed_meta (key, value) VALUES ('model', ?)
          ON CONFLICT(key) DO UPDATE SET value = excluded.value`,
        [modelId],
      );
    }
    d.run(`CREATE VIRTUAL TABLE IF NOT EXISTS vec_index USING vec0(embedding float[${dims}])`);
    db = d;
    return d;
  };

  const ready = (): Promise<Database> => (init ??= open());

  return {
    async drain(): Promise<number> {
      let d: Database;
      try {
        d = await ready();
      } catch {
        // Model unavailable (offline first boot) — try again next tick.
        init = undefined;
        return 0;
      }
      try {
        // Counted by delta, not `changes`: an insert into a vec0 virtual table
        // reports its SHADOW-table writes too (4 rows came back as 14).
        const count = () =>
          (d.query(`SELECT count(*) AS n FROM vec_index`).get() as { n: number }).n;
        const before = count();
        d.prepare(
          `INSERT INTO vec_index (rowid, embedding)
            SELECT h.id, lembed('embed', h.tags || ' ' || substr(h.cmd, 1, ${DOC_CMD_CHARS}))
              FROM src.command_history h
             WHERE h.id NOT IN (SELECT rowid FROM vec_index)
             ORDER BY h.id
             LIMIT ${DRAIN_BATCH}`,
        ).run();
        return count() - before;
      } catch {
        // A locked writer or a torn attach loses one tick, not the layer.
        return 0;
      }
    },

    async similar(text: string): Promise<unknown[]> {
      const d = await ready();
      return d.prepare(
        `SELECT h.cmd, h.tags, h.repo, h.exit_code, h.ts, round(v.distance, 4) AS distance
           FROM (SELECT rowid, distance FROM vec_index
                  WHERE embedding MATCH lembed('embed', ?)
                  ORDER BY distance LIMIT ${KNN_LIMIT}) v
           JOIN src.command_history h ON h.id = v.rowid
          ORDER BY v.distance`,
      ).all(text);
    },

    close(): void {
      db?.close();
      db = undefined;
      init = undefined;
    },
  };
}
