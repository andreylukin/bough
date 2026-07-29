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
import { readdir } from "node:fs/promises";
import { homedir } from "node:os";
import { isAbsolute, join, resolve } from "node:path";
import { BadRequestError, NotFoundError } from "../errors.ts";
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
    const proc = Bun.spawn(["git", "ls-files", ...extra], {
      cwd: dir,
      stdout: "pipe",
      stderr: "ignore",
    });
    const stdout = await new Response(proc.stdout).text();
    if ((await proc.exited) !== 0) return [];
    return stdout.split("\n").map((p) => p.trim()).filter(Boolean);
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

/**
 * `GET /files?workspace=<dir>` — the same listing for a directory with no session.
 *
 * A conversation that has not run a turn has no session id, and that is exactly
 * the screen where someone types `@` for the first time: the popup opened with
 * zero rows, which reads as broken rather than as empty. The session route cannot
 * answer because there is nothing to look up yet.
 *
 * Taking a path here adds no capability — programs already run as the user with
 * the user's full authority and no sandbox (spec §2), so a directory listing is
 * not a door this opens.
 */
export const listFilesForWorkspaceH: Handler = async (req, _ctx: AppCtx) => {
  const dir = new URL(req.url).searchParams.get("workspace") ?? "";
  if (!dir) throw new BadRequestError("workspace is required");
  return json({ files: await listWorkspaceFiles(dir) });
};

/** The most entries one directory listing returns. */
export const MAX_ENTRIES = 2_000;

/** `~` and `~/x` against the real home; everything else is left alone. */
export function expandTilde(p: string): string {
  if (p === "~") return homedir();
  if (p.startsWith("~/")) return join(homedir(), p.slice(2));
  return p;
}

/**
 * One directory's entries, names only, directories suffixed with `/`.
 *
 * Non-recursive on purpose: this backs `@~/`-style browsing, where the user drills
 * down one segment at a time and a recursive walk of `$HOME` would be a stat storm
 * with no gitignore to save it. Dotfiles are included — the caller filters, because
 * only the caller knows whether the typed prefix started with a dot.
 *
 * An unreadable or missing directory answers empty rather than erroring: a
 * half-typed path is not a mistake, it is the middle of typing.
 */
export async function listDirEntries(dir: string): Promise<string[]> {
  try {
    const out: string[] = [];
    for (const e of await readdir(dir, { withFileTypes: true })) {
      out.push(e.isDirectory() ? `${e.name}/` : e.name);
      if (out.length >= MAX_ENTRIES) break;
    }
    return out.sort();
  } catch {
    return [];
  }
}

/**
 * `GET /fs/entries?dir=<path>[&base=<workspace>]` — one directory, for `@` paths
 * that leave the workspace.
 *
 * The `@` popup's normal candidates come from `git ls-files`, which by construction
 * cannot name a file outside the repo — so `@~/notes/todo.md` completed to nothing
 * and the only way to mention a file elsewhere was to type its full path blind.
 * This is the escape hatch, and it is deliberately one level deep: the client asks
 * for the directory it can already see in what you typed.
 *
 * `base` resolves relative dirs (`./x`, `../x`) against the session's workspace
 * rather than against bough's own cwd, which is not a directory the user is
 * thinking about.
 */
export const listDirEntriesH: Handler = async (req, _ctx: AppCtx) => {
  const q = new URL(req.url).searchParams;
  const raw = q.get("dir") ?? "";
  if (!raw) throw new BadRequestError("dir is required");
  const base = q.get("base") ?? "";
  const expanded = expandTilde(raw);
  const dir = isAbsolute(expanded)
    ? expanded
    : base
    ? resolve(base, expanded)
    : resolve(expanded);
  return json({ entries: await listDirEntries(dir) });
};
