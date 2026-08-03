/**
 * Tag popularity over the command-history memory: the session-start priming note
 * and the per-directory profiles behind the mid-turn hints.
 *
 * Weighting, not raw counts: a tag's weight is `success × recency` — a failing
 * command's tag counts a quarter of a passing one, and weight halves every 30
 * days, so a repo's profile tracks what the user does NOW (the Mem^p lesson:
 * memory that is never deprecated erodes performance). The decay runs in JS
 * rather than SQL so nothing depends on the sqlite build carrying math functions.
 *
 * Cache discipline: the priming note goes into the VOLATILE prompt tier, which is
 * cached per session with a 1h TTL (`llm/client.ts`). Recomputing it per turn
 * would change its text mid-session and bust that cache, so it is memoized per
 * session for the process lifetime.
 */

import { isAbsolute, relative } from "node:path";
import { homedir } from "node:os";
import type { CommandTagRow, Db } from "../types.ts";
import { findGitRoot, isRef, repoIdentity } from "./record.ts";

const HALF_LIFE_MS = 30 * 24 * 60 * 60 * 1000;
/** Rows older than this carry <3% weight — not worth reading. */
const LOOKBACK_MS = 5 * HALF_LIFE_MS;
const TOP_TAGS = 10;

function successFactor(exitCode: number | null): number {
  if (exitCode === 0) return 1;
  if (exitCode === null) return 0.5;
  return 0.25;
}

/** Aggregate rows into per-tag weights. Exported for tests. */
export function tagWeights(rows: CommandTagRow[], now: number): Map<string, number> {
  const weights = new Map<string, number>();
  for (const r of rows) {
    const w = successFactor(r.exitCode) * Math.pow(0.5, (now - r.ts) / HALF_LIFE_MS);
    weights.set(r.tag, (weights.get(r.tag) ?? 0) + w);
  }
  return weights;
}

function top(weights: Map<string, number>, limit: number): string[] {
  return [...weights.entries()]
    .sort((a, b) => b[1] - a[1] || (a[0] < b[0] ? -1 : 1))
    .slice(0, limit)
    .map(([tag]) => tag);
}

/** One tag as the priming note ranks it. Exported for `bough tags`, which shows it. */
export interface RankedTag {
  tag: string;
  /** Success × recency, the raw popularity. */
  weight: number;
  /** How many repos in the memory use it. */
  repos: number;
  /** `weight × idf` — what the order is actually by. */
  score: number;
}

/**
 * Rank tags by how much this project's OWN vocabulary they are, not by raw use.
 *
 * WHY NOT POPULARITY. The grammar is `tool:intent:subject`, and a popularity
 * ranking is dominated by the first two: `git`, `bun`, `rg`, `test` recur in every
 * project, while `composer` or `retention` recur only in the one they belong to. So
 * a top-ten by weight spent most of its ten slots anchoring the model on the
 * dimension where reuse was never in doubt, and none on the dimension where a
 * shared vocabulary actually pays.
 *
 * That anchoring is not free. Social-tagging studies find suggested tags drive quick
 * convergence on a shared vocabulary — the power-law that forms when taggers are
 * left alone does not form the same way once suggestions are shown — so whatever
 * this note lists is what gets reused, and listing tool names buys a narrower
 * vocabulary for nothing.
 *
 * The correction is inverse document frequency over REPOS: `weight × ln(1 + N/n)`.
 * A tag in every repo is damped, one in a single repo is lifted. With one repo in
 * the memory every idf is `ln 2` and the order is exactly the popularity order —
 * which is the honest answer when there is nothing to contrast against, and means
 * this needs no special case for a fresh install.
 */
export function rankTags(
  weights: Map<string, number>,
  spread: { repos: number; byTag: Map<string, number> },
  limit: number,
): RankedTag[] {
  return [...weights.entries()]
    // REFERENCES NEVER RANK. `linear.eng-1234` lives in exactly one repo, so the idf
    // below hands it the maximum boost — and it accumulates real weight, because a
    // ticket is worked over many commands. The two multiply, and the note would open
    // every session by reciting last week's ticket numbers instead of this project's
    // words. They are recalled by name (`bough tags show`, history.sql), which is how
    // an identifier is used; a vocabulary is what this list is for.
    .filter(([tag]) => !isRef(tag))
    .map(([tag, weight]) => {
      const repos = spread.byTag.get(tag) ?? 1;
      return { tag, weight, repos, score: weight * Math.log(1 + spread.repos / repos) };
    })
    .sort((a, b) => b.score - a.score || (a.tag < b.tag ? -1 : 1))
    .slice(0, limit);
}

/** The workspace's memory scope: its enclosing checkout's identity, else its path. */
export function workspaceRepo(workspace: string): string {
  return repoIdentity(findGitRoot(workspace) ?? workspace);
}

/**
 * A scope's most-used tags — whole repo, or one directory of it.
 *
 * A DIRECTORY hint stays on plain popularity: it answers "what has been done in
 * here", where the tool is part of the answer, and the set is already narrow enough
 * that the tool names do not crowd anything out.
 */
function topTags(db: Db, repo: string, now: number, limit: number, dir?: string): string[] {
  const rows = db.commandTagRows(repo, {
    ...(dir === undefined ? {} : { dir }),
    sinceTs: now - LOOKBACK_MS,
  });
  return top(tagWeights(rows, now), limit);
}

/** The workspace repo's tags as the priming note ranks them — see `rankTags`. */
export function topRepoTags(db: Db, workspace: string, now: number, limit = TOP_TAGS): string[] {
  return rankedRepoTags(db, workspaceRepo(workspace), now, limit).map((r) => r.tag);
}

/** The same ranking with its arithmetic attached, for `bough tags` to show. */
export function rankedRepoTags(
  db: Db,
  repo: string,
  now: number,
  limit = TOP_TAGS,
): RankedTag[] {
  const since = now - LOOKBACK_MS;
  const weights = tagWeights(db.commandTagRows(repo, { sinceTs: since }), now);
  return rankTags(weights, db.tagSpread(since), limit);
}

// ---------------------------------------------------------------------------
// The session-start priming note
// ---------------------------------------------------------------------------

/** Per-session memo. Both maps are process-lifetime; bounded below. */
const noteMemo = new Map<string, string | null>();
const primedMemo = new Map<string, Set<string>>();
const MEMO_CAP = 512;

function remember<T>(map: Map<string, T>, key: string, value: T): T {
  if (map.size >= MEMO_CAP) map.clear();
  map.set(key, value);
  return value;
}

/**
 * The volatile-tier note naming this project's popular tags, or null for a
 * project with no history yet (the static examples in prompt/shell.md are the
 * cold-start fallback). Frozen per session — see the module header.
 */
export function tagsNoteFor(
  db: Db,
  sessionId: string,
  workspace: string,
  now: number,
): string | null {
  const hit = noteMemo.get(sessionId);
  if (hit !== undefined) return hit;
  let note: string | null = null;
  try {
    const tags = topRepoTags(db, workspace, now);
    remember(primedMemo, sessionId, new Set(tags));
    if (tags.length > 0) {
      note = `This project's own tag vocabulary — the words it uses that other ` +
        `projects do not: ` + tags.join(", ") +
        `. Reuse these when they fit; coin new ones freely when they do not, ` +
        `especially for the tool and the intent.`;
    }
  } catch {
    // Stats are a garnish; a failure here must not touch the turn.
  }
  return remember(noteMemo, sessionId, note);
}

/** The tag set the session was primed with; empty when priming never ran. */
export function primedTags(sessionId: string): Set<string> {
  return primedMemo.get(sessionId) ?? new Set();
}

/**
 * The primed tags as an ordered list, computing (and freezing) them when this
 * session has none yet — the TUI snapshot's view of the same memo the prompt
 * note uses, so the two surfaces cannot disagree within a session.
 */
export function primedTagsFor(
  db: Db,
  sessionId: string,
  workspace: string,
  now: number,
): string[] {
  tagsNoteFor(db, sessionId, workspace, now);
  return [...primedTags(sessionId)];
}

// ---------------------------------------------------------------------------
// Directory-triggered hints
// ---------------------------------------------------------------------------

/** Cap per session — the first thing to cut if these read as noise. */
const MAX_HINTS_PER_SESSION = 4;

interface HintState {
  seenDirs: Set<string>;
  emitted: number;
}

const hintMemo = new Map<string, HintState>();

/**
 * Hint lines for directories the round newly touched — by `view()` reads or by
 * the paths its shell commands named — when a directory's tag profile DIVERGES
 * from what the session was already primed with. No divergence → no line → no
 * context bloat. Once per directory, at most 4 per session.
 *
 * `absDirs` are ABSOLUTE. Each resolves to its own enclosing checkout, so a
 * session rooted at `~` that starts working on `~/repos/bough` gets THAT repo's
 * profile — the cross-repo case the workspace-scoped version was blind to. The
 * workspace repo's own root is skipped (its profile IS the priming set); a
 * foreign repo's root surfaces its whole-repo tags.
 */
export function dirTagHints(
  db: Db,
  sessionId: string,
  workspace: string,
  absDirs: string[],
  now: number,
): string[] {
  let state = hintMemo.get(sessionId);
  if (!state) state = remember(hintMemo, sessionId, { seenDirs: new Set(), emitted: 0 });
  const primed = primedTags(sessionId);
  const wsRoot = findGitRoot(workspace) ?? workspace;
  const wsRepo = repoIdentity(wsRoot);
  const lines: string[] = [];
  for (const abs of absDirs) {
    if (state.emitted >= MAX_HINTS_PER_SESSION) break;
    if (!isAbsolute(abs) || state.seenDirs.has(abs)) continue;
    state.seenDirs.add(abs);
    try {
      const root = findGitRoot(abs) ?? wsRoot;
      const repo = repoIdentity(root);
      const rel = relative(root, abs);
      if (rel.startsWith("..") || isAbsolute(rel)) continue;
      const atRoot = rel === "" || rel === ".";
      if (repo === wsRepo && atRoot) continue;
      const fresh = topTags(db, repo, now, 5, atRoot ? undefined : rel)
        .filter((t) => !primed.has(t));
      if (fresh.length === 0) continue;
      state.emitted++;
      // Same-repo dirs label as the familiar relative path; a foreign repo
      // labels as its own location, home-abbreviated.
      const label = repo === wsRepo ? rel : abs.replace(homedir(), "~");
      lines.push(
        `[history] tags previously used in ${label}/: ${fresh.join(", ")} — ` +
          `see history.sql() for the commands behind them`,
      );
    } catch {
      // Same contract as everything here: hints never hurt a round.
    }
  }
  return lines;
}

/** Test seam: reset the per-session memos. */
export function resetStatsMemo(): void {
  noteMemo.clear();
  primedMemo.clear();
  hintMemo.clear();
}
