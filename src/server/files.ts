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

// ---- directory search (the new-session dialog's path autocomplete) ---------

export interface DirHit {
  /** Absolute path (what createSession wants). */
  path: string;
  /** Same path with the home dir abbreviated to ~ (what the UI shows). */
  display: string;
  /** Has a .git — the likely session workspaces, ranked and marked in the UI. */
  repo: boolean;
}

const DIR_MAX_SCAN = 6_000; // directories visited before we stop
const DIR_MAX_DEPTH = 3; // below the query's base dir
const DIR_MAX_RESULTS = 20;

/**
 * Fuzzy directory search for the new-session dialog. The query's slash-prefix
 * picks the base to walk (`~/repos/bou` walks ~/repos; a bare fragment walks ~),
 * the remainder subsequence-matches dir paths under it, fzf-style — so "bou"
 * finds ~/repos/bough without spelling the middle. `known` (existing session
 * workspaces) is merged in and matched against the whole query, so a dir that
 * hosted a session before ranks even from an empty prompt. Bounded walk, skips
 * dotdirs and dependency/build dirs, never follows what it can't read.
 */
export function searchDirectories(query: string, known: string[] = []): DirHit[] {
  const home = Deno.env.get("HOME") ?? "/";
  const expand = (p: string) => (p === "~" || p.startsWith("~/")) ? home + p.slice(1) : p;
  const abbrev = (p: string) => p === home || p.startsWith(home + "/") ? "~" + p.slice(home.length) : p;
  const q = expand(query.trim());

  // Split into the deepest EXISTING base dir and the leftover fuzzy fragment.
  let base = home;
  let fragment = q;
  if (q.startsWith("/")) {
    const cut = q.lastIndexOf("/");
    const dir = cut === 0 ? "/" : q.slice(0, cut);
    try {
      if (Deno.statSync(dir).isDirectory) {
        base = dir;
        fragment = q.slice(cut + 1);
      }
    } catch {
      // base doesn't exist — fall through to known-workspace matches only
      base = "";
    }
  }

  const isDir = (p: string): boolean => {
    try {
      return Deno.statSync(p).isDirectory;
    } catch {
      return false;
    }
  };
  const isRepo = (p: string) => isDir(`${p}/.git`);

  // Rank: basename subsequence beats path-only subsequence; prefix on the
  // basename beats both; repos beat plain dirs; then shorter paths.
  const rank = (path: string, frag: string): number => {
    const basename = path.slice(path.lastIndexOf("/") + 1).toLowerCase();
    const f = frag.toLowerCase();
    const onBase = basename.startsWith(f) ? 0 : subseq(f, basename) ? 200 : 1000;
    return onBase + (isRepo(path) ? 0 : 100) + path.length;
  };

  const hits = new Map<string, number>(); // path → rank
  let visited = 0;
  const walk = (dir: string, rel: string, depth: number): void => {
    if (visited >= DIR_MAX_SCAN || depth > DIR_MAX_DEPTH) return;
    let entries: Deno.DirEntry[];
    try {
      entries = [...Deno.readDirSync(dir)];
    } catch {
      return;
    }
    for (const e of entries) {
      if (visited >= DIR_MAX_SCAN) return;
      if (!e.isDirectory || e.name.startsWith(".") || SKIP_DIRS.has(e.name)) continue;
      visited++;
      const abs = `${dir}/${e.name}`;
      const relPath = rel ? `${rel}/${e.name}` : e.name;
      if (subseq(fragment, relPath)) hits.set(abs, rank(abs, fragment));
      // A repo is a workspace, not a container — don't offer its subdirs.
      if (!isRepo(abs)) walk(abs, relPath, depth + 1);
    }
  };
  if (base) walk(base, "", 1);

  // Known session workspaces: match against the whole query, best rank tier
  // (also when the walk already found the dir — proven use beats proximity).
  for (const w of known) {
    const abs = expand(w);
    if (!subseq(q, abs) || !isDir(abs)) continue;
    hits.set(abs, Math.min(hits.get(abs) ?? Infinity, rank(abs, fragment) - 2000));
  }

  return [...hits.entries()]
    .sort((a, b) => a[1] - b[1])
    .slice(0, DIR_MAX_RESULTS)
    .map(([path]) => ({ path, display: abbrev(path), repo: isRepo(path) }));
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
