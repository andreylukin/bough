/**
 * The single source of truth for the `~/.bough` data root and every path under it.
 *
 * The invariant: **no module builds a `~/.bough` path by string concatenation.**
 * Every subpath has a named accessor here, so the layout can be read in one place
 * and a rename is one edit rather than a grep.
 *
 * `BOUGH_HOME` overrides the root. This is not a convenience — it is what lets the
 * rewrite run beside the live install without touching its database, its
 * artifacts, or its schedules (plan §2). Every accessor resolves through
 * `boughHome()`, so setting the env var relocates the whole tree; a test that sets
 * it gets a hermetic root and never writes to the real one.
 *
 * Home resolution uses `node:os` `homedir()`, which falls back to the passwd entry
 * when `$HOME` is unset and never throws. There is no strict variant: the old tree
 * needed one for its sandbox and vcs layers, and there is no sandbox now (spec §17).
 */
import { homedir } from "node:os";
import { join, resolve, sep } from "node:path";
import { PathError } from "./errors.ts";

/** The data root: `$BOUGH_HOME`, else `~/.bough`. */
export function boughHome(): string {
  const override = Deno.env.get("BOUGH_HOME");
  if (override && override.trim()) return override;
  return join(homedir(), ".bough");
}

/** A path under the data root. */
export function boughPath(...segs: string[]): string {
  return join(boughHome(), ...segs);
}

// ---- the layout -------------------------------------------------------------

/** The SQLite database. `BOUGH_DB` overrides it outright (`:memory:` in tests). */
export function dbPath(): string {
  return Deno.env.get("BOUGH_DB") ?? boughPath("bough.db");
}

/**
 * Published artifacts, one directory per session. Deliberately OUTSIDE the
 * workspace so publishing never pollutes the diff under review (spec §11). The
 * filesystem is the source of truth here — artifacts survive a database reset.
 */
export function artifactsDir(): string {
  return boughPath("artifacts");
}

/** One session's artifact directory. Names are confined to it — see `confine`. */
export function artifactsDirFor(sessionId: string): string {
  return join(artifactsDir(), sessionId);
}

/**
 * Artifact comment sidecars. Kept OUTSIDE `artifacts/` on purpose: a sidecar
 * inside the artifact directory would show up in every listing and be served as an
 * artifact itself (plan §6.12).
 */
export function commentsDir(): string {
  return boughPath("comments");
}

/** One session's comment sidecar file. */
export function commentsPathFor(sessionId: string): string {
  return join(commentsDir(), `${sessionId}.json`);
}

/**
 * Image bytes referenced by `image` parts. Stored here rather than inline in the
 * parts JSON so message rows stay small and replay survives a moved source file.
 */
export function attachmentsDir(): string {
  return boughPath("attachments");
}

/** Workflow scripts, mirrored per run so they can be edited on disk (spec §8). */
export function workflowsDir(): string {
  return boughPath("workflows");
}

/** The mirror of one run's script: `~/.bough/workflows/<id>.js`. */
export function workflowScriptPath(runId: string): string {
  return join(workflowsDir(), `${runId}.js`);
}

/** User skills. Bundled skills win on a name collision (spec §16). */
export function userSkillsDir(): string {
  return boughPath("skills");
}

/** The persisted theme (a named partial palette). Served over HTTP to the TUI. */
export function themePath(): string {
  return boughPath("theme.json");
}

/** The launcher env file — provider keys and the default model live here. */
export function envPath(): string {
  return boughPath("env");
}

/** The MCP server registry (local stdio and remote entries). */
export function mcpRegistryPath(): string {
  return boughPath("mcp.json");
}

/** OAuth tokens for remote MCP servers, keyed by server name. */
export function mcpAuthPath(): string {
  return boughPath("mcp-auth.json");
}

/** Server logs. */
export function logsDir(): string {
  return boughPath("logs");
}

// ---- confinement ------------------------------------------------------------

/**
 * Resolve `candidate` against `root` and return the absolute path, or throw if it
 * escapes.
 *
 * What this is NOT: a security boundary. Programs run as the user with the user's
 * full authority and can reach any path they like (spec §2). What it IS: the guard
 * on paths the *server* builds from untrusted-ish input — an artifact name, a
 * session id in a URL, a skill directory — so a `..` in a request cannot make the
 * server read or serve a file outside the store it meant to use.
 *
 * Contract:
 *   - Returns an absolute, normalized path that is `root` or strictly beneath it.
 *   - Throws `PathError` on `..` traversal, on an absolute `candidate` outside
 *     `root`, and on a resolved path that leaves `root` by any other route.
 *   - The check is on the RESOLVED path, so a chain of segments that individually
 *     look harmless but resolve outward is still rejected.
 *
 * Two properties follow from that contract and are load-bearing for callers:
 *
 * **It is purely lexical.** Nothing is stat'd and no symlink is followed, so the
 * answer does not depend on what exists on disk. That is deliberate: `confine` has
 * to work for paths that do not exist yet (publishing a new artifact resolves the
 * name before creating the file), and a resolution that touched the filesystem
 * would be non-deterministic and would need permissions the callers may not hold.
 * The consequence is that a real symlink *inside* `root` pointing outward is
 * accepted — following it is not confinement's job, and it cannot be, since
 * programs already read any path they like with the user's authority (spec §2).
 * What `confine` stops is the case it can stop: a name in a request steering the
 * *server's* own path construction out of the store it meant to use.
 *
 * **`root` and `candidate` must be in the same lexical namespace.** Because no
 * symlink is resolved, `/tmp/x` and a realpath'd `/private/tmp/x` are different
 * roots. Callers pass roots built from `boughPath()`, which keeps them consistent.
 */
export function confine(root: string, candidate: string): string {
  // A NUL byte truncates a path at the syscall boundary, so `a\0../../etc` could
  // pass a lexical check and then name something else entirely. Reject it here
  // rather than letting each caller discover it as an opaque OS error.
  if (root.includes("\0") || candidate.includes("\0")) {
    throw new PathError(
      `path contains a NUL byte: ${JSON.stringify(candidate)} under ` +
        `${JSON.stringify(root)}. Pass a plain path with no control characters.`,
    );
  }
  const base = resolve(root);
  const full = resolve(base, candidate);
  // `base + sep` is what makes `/a/bc` fail against root `/a/b`: a shared string
  // prefix is not containment. The `endsWith` guard keeps a filesystem root ("/")
  // from becoming "//".
  const prefix = base.endsWith(sep) ? base : base + sep;
  if (full !== base && !full.startsWith(prefix)) {
    throw new PathError(
      `path escapes its root: ${JSON.stringify(candidate)} resolves to ${full}, ` +
        `which is outside ${base}. Use a path that stays under ${base} — ` +
        `".." segments and absolute paths outside the root are rejected.`,
    );
  }
  return full;
}
