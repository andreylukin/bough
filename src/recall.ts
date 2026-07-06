/**
 * Recall — semantic search over the whole session forest ("did I solve this
 * before?"), powered by the LOCAL embedder (worker/runtime.ts ensureEmbedder).
 * Nothing leaves the machine: conversations are embedded and matched locally.
 *
 * Indexing is lazy: each recall() call first embeds a bounded batch of the
 * newest not-yet-indexed messages, so the index converges over a few searches
 * with no background job to babysit. Vectors are unit-normalized at write time
 * (cosine = dot product); the scan is in-process over Float32 rows — fine at
 * tens of thousands of messages, revisit if the corpus outgrows that.
 *
 * nomic-embed is an asymmetric retrieval model: documents and queries get their
 * task prefixes ("search_document:" / "search_query:") or quality craters.
 */
import type { Db } from "./db/db.ts";
import type { Message, Part } from "./schema/parts.ts";
import { ensureEmbedder } from "./worker/runtime.ts";
import { workerEmbed } from "./worker/client.ts";

export interface RecallHit {
  sessionId: string;
  messageId: string;
  /** The owning session's title (recall spans archived sessions too). */
  title: string;
  role: string;
  /** Start of the matched message's text. */
  snippet: string;
  /** Cosine similarity in [-1, 1]. */
  score: number;
  ts: number;
}

export interface RecallResult {
  hits: RecallHit[];
  /** Messages embedded by this call (lazy indexing progress). */
  indexed: number;
}

/** Injectable embedding for tests: texts → one vector per text. */
export type Embedder = (texts: string[]) => Promise<number[][]>;

const INDEX_PER_CALL = 256;
const EMBED_BATCH = 32;
const EMBED_CLIP = 1200;
const SNIPPET_CHARS = 160;

/** What a message contributes to the index: its prose, not its tool plumbing. */
export function embeddableText(parts: Part[]): string {
  return parts
    .filter((p): p is Extract<Part, { type: "text" | "reasoning" }> =>
      p.type === "text" || p.type === "reasoning"
    )
    .map((p) => p.text)
    .join("\n")
    .trim()
    .slice(0, EMBED_CLIP);
}

/** Search the forest for `query`, lazily indexing a batch of new messages first. */
export async function recall(
  db: Db,
  query: string,
  k = 8,
  embed: Embedder = defaultEmbedder,
): Promise<RecallResult> {
  const indexed = await indexBatch(db, embed);

  const [q] = await embed([`search_query: ${query.slice(0, EMBED_CLIP)}`]);
  const qv = normalize(Float32Array.from(q));
  const scored = db.allEmbeddings()
    .map((row) => ({ row, score: dot(qv, row.vector) }))
    .sort((a, b) => b.score - a.score)
    .slice(0, k);

  const hits: RecallHit[] = [];
  for (const { row, score } of scored) {
    const m = db.getMessage(row.messageId);
    if (!m) continue;
    hits.push({
      sessionId: row.sessionId,
      messageId: row.messageId,
      title: db.getSession(row.sessionId)?.title ?? "",
      role: m.role,
      snippet: embeddableText(m.parts).slice(0, SNIPPET_CHARS),
      score,
      ts: m.createdAt,
    });
  }
  return { hits, indexed };
}

/** Embed up to INDEX_PER_CALL unindexed messages; textless ones get a dim-0 mark. */
async function indexBatch(db: Db, embed: Embedder): Promise<number> {
  const pending = db.messagesToEmbed(INDEX_PER_CALL);
  const withText: { m: Message; text: string }[] = [];
  for (const m of pending) {
    const text = embeddableText(m.parts);
    if (text) withText.push({ m, text });
    else db.putEmbedding(m.id, m.sessionId, null);
  }
  for (let at = 0; at < withText.length; at += EMBED_BATCH) {
    const batch = withText.slice(at, at + EMBED_BATCH);
    const vectors = await embed(batch.map((b) => `search_document: ${b.text}`));
    batch.forEach((b, i) =>
      db.putEmbedding(b.m.id, b.m.sessionId, normalize(Float32Array.from(vectors[i])))
    );
  }
  return withText.length;
}

function normalize(v: Float32Array): Float32Array {
  let sum = 0;
  for (const x of v) sum += x * x;
  const inv = sum > 0 ? 1 / Math.sqrt(sum) : 0;
  for (let i = 0; i < v.length; i++) v[i] *= inv;
  return v;
}

function dot(a: Float32Array, b: Float32Array): number {
  const n = Math.min(a.length, b.length);
  let s = 0;
  for (let i = 0; i < n; i++) s += a[i] * b[i];
  return s;
}

async function defaultEmbedder(texts: string[]): Promise<number[][]> {
  const url = await ensureEmbedder();
  return await workerEmbed(url, texts);
}
