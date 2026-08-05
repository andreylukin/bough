/**
 * Offline A/B of the priming-note ranking, against the real command memory.
 *
 * The question the note exists to answer is "which tags should the model reuse
 * next", so the evaluation is exactly that: rank on everything before a cut, then
 * measure the ranking against the tags actually used AFTER it. Self-contained —
 * both rankings are reimplemented here so it runs against any checkout's database.
 *
 * READ THE HORIZON BEFORE READING THE NUMBERS. The note is frozen for a session's
 * lifetime, and on the corpus this was built against 124 of 127 sessions span under
 * two hours (mean 13 minutes, max 4). So `horizon 2h` is the row that describes how
 * the note is actually used; 24h describes a session that does not exist. The
 * models disagree across that range and the ranking inverts, so quoting the wrong
 * row gets the opposite answer.
 *
 * Result on 6,795 commands / 28 repos / 3 days, 2026-08-05:
 *
 *     horizon 2h    prec@10   MRR    paired vs exp
 *     freq           53.4%   0.868      6/3
 *     exp            53.0%   0.853       —
 *     bll            54.4%   0.888     16/8      <- best where it matters
 *
 *     horizon 24h   prec@10   MRR    paired vs exp
 *     freq           74.9%   0.915     10/0      <- and here, where it does not
 *     exp            73.7%   0.915       —
 *     bll            73.6%   0.909      7/8
 *
 * Small effects on small n — 79 cuts at 2h, ~1.4pt of precision. The paired counts
 * are the more trustworthy signal than the means. What is NOT small: the shipped
 * exponential is last or tied at every horizon, which is the finding that matters.
 *
 * usage: bun ab.ts [/path/to/bough.db]
 */
import { Database } from "bun:sqlite";

const DAY = 86_400_000;
const dbPath = process.argv[2] ?? `${process.env.HOME}/.bough/bough.db`;
const db = new Database(dbPath, { readonly: true });
db.run("PRAGMA query_only = ON");

type Row = { repo: string; tag: string; ts: number; exit_code: number | null };
const rows = db.query<Row, []>(
  `SELECT h.repo AS repo, t.tag AS tag, h.ts AS ts, h.exit_code AS exit_code
     FROM command_history h JOIN command_tags t ON t.command_id = h.id
    ORDER BY h.ts`,
).all();

const success = (e: number | null) => (e === 0 ? 1 : e === null ? 0.5 : 0.25);
const isRef = (t: string) => t.includes(".");

/** The shipped ranking before this change: exponential decay, 30-day half life. */
function expWeights(rs: Row[], now: number): Map<string, number> {
  const w = new Map<string, number>();
  for (const r of rs) {
    w.set(r.tag, (w.get(r.tag) ?? 0) + success(r.exit_code) * Math.pow(0.5, (now - r.ts) / (30 * DAY)));
  }
  return w;
}

/** ACT-R base-level activation: Σ t^-0.5, floored at an hour. */
function bllWeights(rs: Row[], now: number): Map<string, number> {
  const w = new Map<string, number>();
  for (const r of rs) {
    const elapsed = Math.max(now - r.ts, 3_600_000) / DAY;
    w.set(r.tag, (w.get(r.tag) ?? 0) + success(r.exit_code) * Math.pow(elapsed, -0.5));
  }
  return w;
}

/** Raw frequency, no time term at all — the paper's MP baseline. */
function freqWeights(rs: Row[]): Map<string, number> {
  const w = new Map<string, number>();
  for (const r of rs) w.set(r.tag, (w.get(r.tag) ?? 0) + 1);
  return w;
}

function rank(weights: Map<string, number>, spread: Map<string, number>, repos: number, k: number) {
  return [...weights.entries()]
    .filter(([t]) => !isRef(t))
    .map(([tag, weight]) => ({ tag, score: weight * Math.log(1 + repos / (spread.get(tag) ?? 1)) }))
    .sort((a, b) => b.score - a.score || (a.tag < b.tag ? -1 : 1))
    .slice(0, k)
    .map((r) => r.tag);
}

// Repo spread is global and fixed, as in production.
const spread = new Map<string, number>();
{
  const byTag = new Map<string, Set<string>>();
  for (const r of rows) {
    let s = byTag.get(r.tag);
    if (!s) byTag.set(r.tag, (s = new Set()));
    s.add(r.repo);
  }
  for (const [t, s] of byTag) spread.set(t, s.size);
}
const nRepos = new Set(rows.map((r) => r.repo)).size;

const K = 10;
/** Every cut is a real session start: a repo, and a moment it had history. */
const byRepo = new Map<string, Row[]>();
for (const r of rows) {
  let a = byRepo.get(r.repo);
  if (!a) byRepo.set(r.repo, (a = []));
  a.push(r);
}

const HORIZONS = [2, 6, 24]; // hours of future work the note is judged against
const models = ["freq", "exp", "bll"] as const;

for (const H of HORIZONS) {
const HORIZON = H * 3_600_000;
const hits: Record<string, number> = { freq: 0, exp: 0, bll: 0 };
const recall: Record<string, number> = { freq: 0, exp: 0, bll: 0 };
const mrr: Record<string, number> = { freq: 0, exp: 0, bll: 0 };
// Paired: how often each model strictly beats the shipped `exp` on the same cut.
const winVsExp: Record<string, number> = { freq: 0, exp: 0, bll: 0 };
const loseVsExp: Record<string, number> = { freq: 0, exp: 0, bll: 0 };
let cuts = 0;

for (const [repo, rs] of byRepo) {
  if (rs.length < 100) continue; // a repo with no history has nothing to rank
  const t0 = rs[0].ts, tEnd = rs[rs.length - 1].ts;
  // Cuts every hour across the repo's life, each needing past and future.
  for (let cut = t0 + 3_600_000; cut < tEnd - HORIZON; cut += 3_600_000) {
    const past = rs.filter((r) => r.ts <= cut && r.ts > cut - 150 * DAY);
    const future = new Set(
      rs.filter((r) => r.ts > cut && r.ts <= cut + HORIZON && !isRef(r.tag)).map((r) => r.tag),
    );
    if (past.length < 30 || future.size === 0) continue;
    cuts++;
    const ranked: Record<string, string[]> = {
      freq: rank(freqWeights(past), spread, nRepos, K),
      exp: rank(expWeights(past, cut), spread, nRepos, K),
      bll: rank(bllWeights(past, cut), spread, nRepos, K),
    };
    const hitOf: Record<string, number> = {};
    for (const m of models) {
      const r = ranked[m];
      const hit = r.filter((t) => future.has(t)).length;
      hitOf[m] = hit;
      hits[m] += hit / K; // precision@10: how much of the note is worth its space
      recall[m] += hit / Math.min(future.size, K);
      const first = r.findIndex((t) => future.has(t));
      mrr[m] += first < 0 ? 0 : 1 / (first + 1);
    }
    for (const m of models) {
      if (hitOf[m] > hitOf.exp) winVsExp[m]++;
      else if (hitOf[m] < hitOf.exp) loseVsExp[m]++;
    }
  }
}

console.log(`\nhorizon ${H}h — cuts: ${cuts}  repos: ${byRepo.size}  K=${K}`);
console.log("model  precision@10  recall@10   MRR   vs exp (win/loss)");
for (const m of models) {
  const p = (hits[m] / cuts * 100).toFixed(1);
  const r = (recall[m] / cuts * 100).toFixed(1);
  const mr = (mrr[m] / cuts).toFixed(3);
  const paired = m === "exp" ? "—" : `${winVsExp[m]}/${loseVsExp[m]}`;
  console.log(`${m.padEnd(6)} ${p.padStart(11)}% ${r.padStart(9)}% ${mr.padStart(7)}   ${paired}`);
}
}
