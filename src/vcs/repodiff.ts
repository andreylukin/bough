/**
 * The Changes rail's repo source: what THIS session changed in the user's checkout.
 *
 * There is no snapshot substrate under this. The agent's tools and its bash both run
 * directly in the checkout, so the working tree IS the tip — the only thing worth
 * recording is where the session started. `sessions.base` holds that sha (written by
 * the workspace prep on the first turn), and the change set is simply
 * `git diff <base>` plus whatever is untracked.
 *
 * Why so little: the previous substrates (shadow worktrees, then an agentfs
 * copy-on-write delta) existed to keep the agent's edits out of the user's tree and
 * then carry them back in. That bought isolation git already provides for a repo and
 * paid for it by breaking git — see shellInvocation in tools/bash.ts. With the agent
 * working in the checkout like a human, `git diff` is the review payload and
 * `git commit`/`git push` are the delivery mechanism, so materialize/ship/accept
 * have nothing left to do.
 *
 * Revert is the one real mutation here: restore tracked paths from the base sha and
 * delete untracked ones. It is per-path and never touches anything the session did
 * not change.
 */
import { dirname, join } from "node:path";
import { type Diff, type FileDiff, parseGitDiff } from "../schema/changes.ts";

async function git(
  dir: string,
  args: string[],
): Promise<{ ok: boolean; out: string; err: string }> {
  const { code, stdout, stderr } = await new Deno.Command("git", {
    args: ["-C", dir, ...args],
    stdin: "null",
    stdout: "piped",
    stderr: "piped",
  }).output();
  const dec = new TextDecoder();
  return { ok: code === 0, out: dec.decode(stdout), err: dec.decode(stderr) };
}

/** Whether `dir` is inside a git work tree. */
export async function isRepo(dir: string): Promise<boolean> {
  const r = await git(dir, ["rev-parse", "--is-inside-work-tree"]);
  return r.ok && r.out.trim() === "true";
}

/** The checkout's current HEAD sha, or null (no commits yet / not a repo). */
export async function headSha(dir: string): Promise<string | null> {
  const r = await git(dir, ["rev-parse", "--verify", "-q", "HEAD"]);
  return r.ok ? r.out.trim() : null;
}

/** Paths git doesn't track and isn't ignoring, relative to the repo root. */
async function untracked(dir: string): Promise<string[]> {
  const r = await git(dir, ["ls-files", "--others", "--exclude-standard"]);
  if (!r.ok) return [];
  return r.out.split("\n").map((l) => l.trim()).filter(Boolean);
}

/** Render an untracked file as an all-added FileDiff (binary/unreadable → no hunks). */
async function addedFile(dir: string, path: string): Promise<FileDiff> {
  let lines: string[] = [];
  try {
    const text = await Deno.readTextFile(join(dir, path));
    // Trailing "" from a final newline isn't a line; a file with no trailing
    // newline keeps its last line.
    const body = text.split("\n");
    if (body.at(-1) === "") body.pop();
    lines = body.map((l) => `+${l}`);
  } catch {
    lines = []; // binary, unreadable, or vanished mid-review
  }
  return {
    path,
    status: "added",
    hunks: lines.length ? [{ header: `@@ -0,0 +1,${lines.length} @@`, lines }] : [],
  };
}

/**
 * What this session changed in `dir` since `base`: tracked edits from `git diff`
 * plus untracked files as additions. A missing/unresolvable base (a non-repo dir,
 * or a session that predates the base column) degrades to an empty diff rather than
 * reporting the user's whole tree as the agent's work.
 */
export async function diffSince(dir: string, base: string | null): Promise<Diff> {
  if (!base || !(await isRepo(dir))) return { source: "repo", files: [] };
  const r = await git(dir, ["diff", "--no-color", "--no-ext-diff", base]);
  if (!r.ok) return { source: "repo", files: [] };
  const files = parseGitDiff(r.out);
  for (const path of await untracked(dir)) files.push(await addedFile(dir, path));
  return { source: "repo", files };
}

/**
 * Undo the session's work on `paths`: tracked files are restored to their content at
 * `base`, untracked files are deleted. Paths the session never changed are left
 * alone by construction — the caller passes what the diff reported. Returns the
 * paths actually reverted.
 */
export async function revertPaths(
  dir: string,
  base: string | null,
  paths: string[],
): Promise<string[]> {
  if (!base) return [];
  const done: string[] = [];
  for (const path of paths) {
    // Tracked at base → restore that content. `git checkout <sha> -- <path>` also
    // stages it, which is what a reviewer means by "put it back".
    const known = await git(dir, ["cat-file", "-e", `${base}:${path}`]);
    if (known.ok) {
      const r = await git(dir, ["checkout", base, "--", path]);
      if (r.ok) done.push(path);
      continue;
    }
    // Not in the base commit → the session created it. Delete it, and prune the
    // directories that existed only to hold it.
    try {
      await Deno.remove(join(dir, path));
      done.push(path);
      let parent = dirname(path);
      while (parent && parent !== "." && parent !== "/") {
        try {
          await Deno.remove(join(dir, parent)); // fails once non-empty — that's the stop
        } catch {
          break;
        }
        parent = dirname(parent);
      }
    } catch { /* already gone */ }
  }
  return done;
}
