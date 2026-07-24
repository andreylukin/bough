/**
 * Workspace file search for the composer's `@` autocomplete. A shallow, bounded
 * walk of the session's workspace with a subsequence match (the fuzzy-find users
 * expect: "arts" matches "parts.ts"). Pruned to stay cheap and never leak the
 * machine's whole disk: skips VCS/dependency/build dirs and dotfiles, caps the
 * number of files scanned and results returned. Read-only; no traversal outside
 * the workspace root (paths are returned workspace-relative).
 *
 * Also home to the composer's `@path` reference expansion: text files inline at
 * replay time (expandFileReferences) while image files become attachment-backed
 * image parts at compose time (collectImageAttachments) — see schema/parts.ts.
 */
import type { ImagePart } from "../schema/parts.ts";

const SKIP_DIRS = new Set([
  ".git",
  ".jj",
  "node_modules",
  ".deno",
  "dist",
  "build",
  "target",
  ".next",
  ".cache",
  "vendor",
  "__pycache__",
  ".venv",
  "venv",
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
  const abbrev = (p: string) =>
    p === home || p.startsWith(home + "/") ? "~" + p.slice(home.length) : p;
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
  // Require .git/HEAD, not merely a .git node — a bare/empty `.git` dir (or a
  // stray one) would otherwise mislabel a plain folder as a repo.
  const isRepo = (p: string): boolean => {
    try {
      return Deno.statSync(`${p}/.git/HEAD`).isFile;
    } catch {
      return false;
    }
  };

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

/**
 * Directories the agent user has been granted access to (`bough grant <dir>`),
 * read from ~/.bough/grants.json. In agent-user mode the server's own $HOME is
 * near-empty, so these are the real roots for the new-session picker. Returns []
 * when the file is absent (single-user mode) or unreadable.
 */
export function grantedDirs(): string[] {
  const home = Deno.env.get("HOME");
  if (!home) return [];
  try {
    const raw = Deno.readTextFileSync(`${home}/.bough/grants.json`);
    const arr = JSON.parse(raw);
    return Array.isArray(arr) ? arr.filter((p): p is string => typeof p === "string") : [];
  } catch {
    return [];
  }
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
    // Images ride as attachment-backed image parts (collectImageAttachments),
    // never as inlined "text" — a binary read here would be mojibake.
    if (imageMediaType(rel)) continue;
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

// ---- image attachments (`@shot.png` → image parts) --------------------------

/** The formats we attach (matches what the Anthropic API accepts). */
const IMAGE_TYPES: Record<string, string> = {
  png: "image/png",
  jpg: "image/jpeg",
  jpeg: "image/jpeg",
  gif: "image/gif",
  webp: "image/webp",
};

/** Media type for a path with a supported image extension, else undefined. */
function imageMediaType(path: string): string | undefined {
  return IMAGE_TYPES[path.slice(path.lastIndexOf(".") + 1).toLowerCase()];
}

/** Anthropic's per-image cap; a larger file stays a plain @reference. */
const MAX_IMAGE_BYTES = 5 * 1024 * 1024;

/** Where attached images are copied: ~/.bough/attachments. */
function attachmentsDir(): string {
  return `${Deno.env.get("HOME") ?? "."}/.bough/attachments`;
}

/**
 * Collect `@path` image references from a user message into image parts, copying
 * each file to `destDir` at compose time (the part stores the copy's path — the
 * message replays even after the original moves; see schema/parts.ts). Relative
 * refs resolve against the workspace with the same `..` confinement as
 * expandFileReferences; absolute and `~/` refs are ALSO accepted for images —
 * a screenshot usually lives outside the repo, and the user is explicitly
 * pointing at it. Missing, oversized, or uncopyable files are skipped (the raw
 * @ref stays in the text for the agent's own tools). Never throws.
 */
export function collectImageAttachments(
  text: string,
  workspace: string | null,
  destDir: string = attachmentsDir(),
): ImagePart[] {
  const home = Deno.env.get("HOME");
  const seen = new Set<string>();
  const parts: ImagePart[] = [];
  // Same token shape as expandFileReferences, plus `~` so "@~/Desktop/x.png" works.
  for (const m of text.matchAll(/(?:^|\s)@([\w./~-]+)/g)) {
    const ref = m[1];
    const mediaType = imageMediaType(ref);
    if (!mediaType || seen.has(ref)) continue;
    seen.add(ref);
    let abs: string;
    if (ref.startsWith("/")) abs = ref;
    else if (home && ref.startsWith("~/")) abs = home + ref.slice(1);
    else if (workspace && !ref.includes("..")) abs = `${workspace.replace(/\/+$/, "")}/${ref}`;
    else continue;
    const part = attachImageFile(abs, ref, destDir);
    if (part) parts.push(part);
  }
  return parts;
}

/**
 * Copy one image file into the attachment store and describe it as an ImagePart.
 * The single place that enforces the attachment rules (supported extension,
 * regular file, ≤ MAX_IMAGE_BYTES) and the copy-then-store-the-copy discipline.
 * Returns null — never throws — when the file cannot be attached; callers decide
 * whether that is a skip (composer @refs) or an error (the image() host fn).
 * `abs` must already be absolute; `name` is the label shown in the transcript.
 */
export function attachImageFile(
  abs: string,
  name: string,
  destDir: string = attachmentsDir(),
): ImagePart | null {
  const mediaType = imageMediaType(abs);
  if (!mediaType) return null;
  try {
    const info = Deno.statSync(abs);
    if (!info.isFile || info.size > MAX_IMAGE_BYTES) return null;
    Deno.mkdirSync(destDir, { recursive: true });
    const dest = `${destDir}/${crypto.randomUUID()}.${
      abs.slice(abs.lastIndexOf(".") + 1).toLowerCase()
    }`;
    Deno.copyFileSync(abs, dest);
    return { type: "image", path: dest, mediaType, name, size: info.size };
  } catch {
    return null; // missing / unreadable
  }
}

/**
 * Same as attachImageFile, but from bytes already in hand — for images that do
 * not exist on the host tree at all. The agentfs overlay is the reason: a file the
 * program just wrote lives in the session's copy-on-write delta, so statting the
 * host path finds nothing and the bytes have to be read back through the overlay
 * first. `name` supplies the extension (hence the media type), exactly as the
 * path does in the file variant.
 */
export function attachImageBytes(
  bytes: Uint8Array,
  name: string,
  destDir: string = attachmentsDir(),
): ImagePart | null {
  const mediaType = imageMediaType(name);
  if (!mediaType || bytes.byteLength > MAX_IMAGE_BYTES) return null;
  try {
    Deno.mkdirSync(destDir, { recursive: true });
    const dest = `${destDir}/${crypto.randomUUID()}.${
      name.slice(name.lastIndexOf(".") + 1).toLowerCase()
    }`;
    Deno.writeFileSync(dest, bytes);
    return { type: "image", path: dest, mediaType, name, size: bytes.byteLength };
  } catch {
    return null; // attachment store unwritable
  }
}

/**
 * An image part → the base64 block replayed to the LLM (turn.ts history
 * assembly). A missing or unreadable attachment degrades to a text placeholder —
 * history must always replay, never crash the turn.
 */
export function imagePartToBlock(
  part: ImagePart,
): { type: "image"; data: string; mediaType: string; name: string } | {
  type: "text";
  text: string;
} {
  try {
    return {
      type: "image",
      data: Deno.readFileSync(part.path).toBase64(),
      mediaType: part.mediaType,
      name: part.name,
    };
  } catch {
    return { type: "text", text: `[image: ${part.name} — attachment missing]` };
  }
}
