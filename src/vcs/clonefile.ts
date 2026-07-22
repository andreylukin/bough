/**
 * Non-git config snapshots via APFS `clonefile` (`cp -c`). For files outside a
 * repo — `~/.zshrc`, `~/.config`, etc. — there's no git history, so we snapshot by
 * cloning the originals into a per-session dir. The agent edits the *clones* (the
 * seatbelt sandbox denies writes to the real config paths but allows the snapshot
 * dir), the reviewer sees a `git diff --no-index` of pristine-original vs. edited
 * clone, and approved files are copied back over the originals.
 *
 * APFS clonefile makes the initial copy O(1) and space-free (copy-on-write), so
 * snapshotting a large `~/.config` is cheap. macOS/APFS only.
 *
 * Layout: a clone mirrors its original's absolute path under the session dir, so
 * the mapping is reversible with no side table — clone = `<base>/<sessionId>` +
 * `<originalAbsPath>`. A small manifest records which top-level paths were
 * snapshotted so `diff` knows what to compare.
 */
import { type Diff, parseGitDiff } from "../schema/changes.ts";
import { pathExists } from "../fsutil.ts";
import { homeStrict } from "../paths.ts";

export const MANIFEST = ".bough-manifest.json";

/** Default snapshots root: `~/.bough/snapshots`. */
export function snapshotBase(): string {
  return `${homeStrict("clonefile")}/.bough/snapshots`;
}

/** The dir holding one session's clones. `base` overrides the default root (tests). */
export function sessionDir(sessionId: string, base: string = snapshotBase()): string {
  return `${base}/${sessionId}`;
}

/** Clone path for an original absolute path: the original mirrored under the session dir. */
function cloneOf(sessionId: string, origAbs: string, base: string): string {
  // origAbs is absolute (leading "/"); join keeps the full path under the session dir.
  return `${sessionDir(sessionId, base)}${origAbs}`;
}

interface RunResult {
  ok: boolean;
  code: number;
  stdout: string;
  stderr: string;
}

async function run(bin: string, args: string[]): Promise<RunResult> {
  const { code, stdout, stderr } = await new Deno.Command(bin, {
    args,
    stdout: "piped",
    stderr: "piped",
  }).output();
  return {
    ok: code === 0,
    code,
    stdout: new TextDecoder().decode(stdout),
    stderr: new TextDecoder().decode(stderr),
  };
}

function parentOf(path: string): string {
  const i = path.lastIndexOf("/");
  return i <= 0 ? "/" : path.slice(0, i);
}

/** APFS clonefile copy (`cp -c`, recursive for dirs). Throws on failure. */
async function clone(src: string, dst: string): Promise<void> {
  await Deno.mkdir(parentOf(dst), { recursive: true });
  const info = await Deno.stat(src);
  const args = info.isDirectory ? ["-cR", src, dst] : ["-c", src, dst];
  const r = await run("cp", args);
  if (!r.ok) throw new Error(`cp ${args.join(" ")} failed: ${r.stderr.trim()}`);
}

/**
 * Clone each original path into the session snapshot dir and record the set.
 * `paths` are absolute. Returns a map of original → clone path. The agent then
 * edits the clones; the originals stay pristine for the diff.
 */
export async function snapshotPaths(
  sessionId: string,
  paths: string[],
  base: string = snapshotBase(),
): Promise<Record<string, string>> {
  const map: Record<string, string> = {};
  for (const orig of paths) {
    const dst = cloneOf(sessionId, orig, base);
    await clone(orig, dst);
    map[orig] = dst;
  }
  const dir = sessionDir(sessionId, base);
  await Deno.mkdir(dir, { recursive: true });
  await Deno.writeTextFile(`${dir}/${MANIFEST}`, JSON.stringify(paths, null, 2));
  return map;
}

async function readManifest(sessionId: string, base: string): Promise<string[]> {
  try {
    return JSON.parse(await Deno.readTextFile(`${sessionDir(sessionId, base)}/${MANIFEST}`));
  } catch {
    return [];
  }
}

/**
 * Structured diff of pristine originals vs. edited clones, via
 * `git diff --no-index` per snapshotted path (git recurses into dirs, so adds /
 * deletes / modifies within a snapshotted dir all surface). `FileDiff.path` is the
 * original absolute path.
 */
export async function diff(sessionId: string, base: string = snapshotBase()): Promise<Diff> {
  const dir = sessionDir(sessionId, base);
  // git strips the leading "/" from each side; recover the original abs path by
  // dropping the snapshot-dir prefix (present only on the clone side) and re-adding "/".
  const snapNoSlash = dir.replace(/^\//, "");
  const strip = (p: string): string =>
    "/" + (p.startsWith(snapNoSlash + "/") ? p.slice(snapNoSlash.length + 1) : p);

  let out = "";
  for (const orig of await readManifest(sessionId, base)) {
    const cl = cloneOf(sessionId, orig, base);
    // --no-index exits 1 when files differ (not an error); >1 is a real failure.
    const r = await run("git", ["diff", "--no-index", "--no-color", orig, cl]);
    if (r.code > 1) throw new Error(`git diff --no-index failed: ${r.stderr.trim()}`);
    out += r.stdout;
  }
  return { source: "clonefile", files: parseGitDiff(out, strip) };
}

/**
 * Copy approved edits back over their originals. `approvedFiles` are original
 * absolute paths (as reported by `diff`). A file the agent deleted (clone gone)
 * is removed from the original; otherwise the clone is cloned back.
 */
export async function applyBack(
  sessionId: string,
  approvedFiles: string[],
  base: string = snapshotBase(),
): Promise<void> {
  for (const orig of approvedFiles) {
    const cl = cloneOf(sessionId, orig, base);
    if (await pathExists(cl)) {
      await clone(cl, orig);
    } else if (await pathExists(orig)) {
      await Deno.remove(orig);
    }
  }
}
