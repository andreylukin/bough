/**
 * Workspace file listing, for the composer's `@` completion.
 *
 * THE INVARIANT THIS HOLDS: **the candidate list is what git tracks, plus what git
 * would let you add.** `git ls-files --cached --others --exclude-standard` is the
 * whole implementation, and it is the right one for three reasons that a
 * hand-rolled directory walk gets wrong: it honours `.gitignore` (so `node_modules`
 * and build output never reach the popup), it is one process rather than a
 * recursive stat storm, and it agrees with the Changes rail — a file you can
 * mention is a file you will later review.
 *
 * A workspace that is not a repository answers with an empty list rather than an
 * error. The composer degrades to no suggestions, which is the same experience as
 * a query that matches nothing, and is not worth a modal.
 *
 * Ranking is NOT here. `rankCompletions` in `tui/format.ts` is pure, tested, and
 * already the one place fuzzy order is decided; this route returns paths and the
 * client ranks them. The cap exists so a monorepo cannot stream 200k paths into a
 * terminal on every keystroke.
 */
import { NotFoundError } from "../errors.ts";
import type { AppCtx } from "../types.ts";
import { type Handler, json } from "./http.ts";

/**
 * The most paths one listing returns.
 *
 * Generous — the client fuzzy-filters the whole list, so a cap that bites makes
 * completion silently incomplete rather than slow. Large repos exist; this repo's
 * own `bench/` has 50k files, and the point of the gitignore filter is that they
 * do not get here in the first place.
 */
export const MAX_FILES = 20_000;

/** One `git ls-files` pass. Empty on any failure — a keystroke is not worth an error. */
async function lsFiles(dir: string, extra: string[]): Promise<string[]> {
  try {
    const cmd = new Deno.Command("git", {
      args: ["ls-files", ...extra],
      cwd: dir,
      stdout: "piped",
      stderr: "null",
    });
    const out = await cmd.output();
    if (!out.success) return [];
    return new TextDecoder().decode(out.stdout).split("\n").map((p) => p.trim()).filter(Boolean);
  } catch {
    // No git, or the directory vanished.
    return [];
  }
}

/**
 * Candidates for `@`, repo-relative, tracked first. Empty when `dir` is not a repo.
 *
 * TRACKED FIRST, and that ordering is the whole reason this is two passes instead
 * of one. `--cached --others` in a single call interleaves them alphabetically, so
 * one large untracked directory that nobody has gitignored — build output, a data
 * dir, this repo's own 50k-file `bench/` — reaches the cap before the source files
 * do and the popup offers you nothing you would ever mention. A file you have
 * committed is the file you mean; an untracked one is the tail.
 */
export async function listWorkspaceFiles(dir: string): Promise<string[]> {
  const tracked = await lsFiles(dir, ["--cached"]);
  const out = tracked.slice(0, MAX_FILES);
  if (out.length >= MAX_FILES) return out;
  const seen = new Set(out);
  for (const p of await lsFiles(dir, ["--others", "--exclude-standard"])) {
    if (seen.has(p)) continue;
    out.push(p);
    if (out.length >= MAX_FILES) break;
  }
  return out;
}

/**
 * `GET /sessions/:id/files` — the `@` completion's candidates for that session's
 * workspace.
 *
 * Session-scoped rather than taking a path parameter, because the workspace is
 * already a fact the server owns and a client that could name any directory would
 * be a directory-listing API wearing a completion's clothes.
 */
export const listFilesH: Handler = async (_req, ctx: AppCtx, params) => {
  const session = ctx.db.getSession(params.id);
  if (!session) throw new NotFoundError(`session ${params.id} not found`);
  // A session with no workspace has no files to offer — the same answer as a
  // directory that is not a repository.
  const dir = session.workspace ?? "";
  return json({ files: dir ? await listWorkspaceFiles(dir) : [] });
};
