/**
 * Workspace file search for the composer's `@` autocomplete. A shallow, bounded
 * walk of the session's workspace with a subsequence match (the fuzzy-find users
 * expect: "arts" matches "parts.ts"). Pruned to stay cheap and never leak the
 * machine's whole disk: skips VCS/dependency/build dirs and dotfiles, caps the
 * number of files scanned and results returned. Read-only; no traversal outside
 * the workspace root (paths are returned workspace-relative).
 */

const SKIP_DIRS = new Set([
  ".git", ".jj", "node_modules", ".deno", "dist", "build", "target",
  ".next", ".cache", "vendor", "__pycache__", ".venv", "venv",
]);
const MAX_SCAN = 20_000; // files walked before we stop
const MAX_RESULTS = 20;

/** Subsequence match: every char of `q` appears in `s` in order (case-insensitive). */
function subseq(q: string, s: string): boolean {
  if (!q) return true;
  let i = 0;
  const ql = q.toLowerCase();
  const sl = s.toLowerCase();
  for (let j = 0; j < sl.length && i < ql.length; j++) {
    if (sl[j] === ql[i]) i++;
  }
  return i === ql.length;
}

/** Rank: prefer a hit on the basename, then a shorter path. Lower is better. */
function score(rel: string, q: string): number {
  const base = rel.slice(rel.lastIndexOf("/") + 1);
  const onBase = subseq(q, base) ? 0 : 1000;
  return onBase + rel.length;
}

/**
 * Return up to MAX_RESULTS workspace-relative paths matching `query`. `query` ""
 * lists the first files found (useful for an initial dropdown). Non-existent or
 * unreadable roots yield [].
 */
export async function searchWorkspaceFiles(root: string, query: string): Promise<string[]> {
  const q = query.trim();
  const hits: string[] = [];
  let scanned = 0;

  async function walk(dir: string, prefix: string): Promise<void> {
    if (scanned >= MAX_SCAN) return;
    let entries: Deno.DirEntry[];
    try {
      entries = [...Deno.readDirSync(dir)];
    } catch {
      return;
    }
    for (const e of entries) {
      if (scanned >= MAX_SCAN) return;
      if (e.name.startsWith(".") || SKIP_DIRS.has(e.name)) continue;
      const rel = prefix ? `${prefix}/${e.name}` : e.name;
      if (e.isDirectory) {
        await walk(`${dir}/${e.name}`, rel);
      } else if (e.isFile) {
        scanned++;
        if (subseq(q, rel)) hits.push(rel);
      }
    }
  }

  await walk(root, "");
  hits.sort((a, b) => score(a, q) - score(b, q));
  return hits.slice(0, MAX_RESULTS);
}

const MAX_INLINE_BYTES = 64 * 1024;

/**
 * Expand `@path` references in a user message into inlined file content, so the
 * model actually sees the file the user pointed at (not just its name). Read-only
 * and confined to the workspace: a token that escapes the root, doesn't exist, or
 * is too big is left as-is (the agent can still read it with its tools). Returns
 * the original text plus one appended <file> block per resolved reference.
 */
export function expandFileReferences(text: string, workspace: string): string {
  const root = workspace.replace(/\/+$/, "");
  const seen = new Set<string>();
  const blocks: string[] = [];
  for (const m of text.matchAll(/(?:^|\s)@([\w./-]+)/g)) {
    const rel = m[1];
    if (seen.has(rel) || rel.includes("..")) continue;
    seen.add(rel);
    const abs = `${root}/${rel}`;
    try {
      const info = Deno.statSync(abs);
      if (!info.isFile || info.size > MAX_INLINE_BYTES) continue;
      const content = Deno.readTextFileSync(abs);
      blocks.push(`<file path="${rel}">\n${content}\n</file>`);
    } catch {
      // missing / unreadable — leave the @reference for the agent's own tools
    }
  }
  return blocks.length ? `${text}\n\n${blocks.join("\n\n")}` : text;
}
