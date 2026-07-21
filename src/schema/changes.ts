/**
 * The Changes-tab contract — Zod schemas for a structured diff, shared by both
 * snapshot sources (shadow for repo work, clonefile for non-git config). The UI's
 * Changes rail renders a `Diff` and lets the reviewer approve/apply per file or
 * per hunk, so this shape is deliberately minimal and source-tagged.
 *
 * Design notes:
 *   - A `Diff` carries its `source` so the apply path knows which backend to call
 *     (shadow materialize/restore vs. clonefile copy-back) — the shapes are otherwise identical.
 *   - `FileDiff.status` is the coarse git status (added/modified/deleted). Renames
 *     surface as a delete + add pair (git's default without `-M`), keeping the model flat.
 *   - A `Hunk` is one `@@ ... @@` block: its `header` verbatim plus the body `lines`
 *     with their leading ` `/`+`/`-` markers intact, so the UI can colour them without
 *     re-parsing. Binary/empty files yield a FileDiff with no hunks.
 */
import { z } from "zod";

export const FileStatus = z.enum(["added", "modified", "deleted"]);
export type FileStatus = z.infer<typeof FileStatus>;

/** One `@@ -a,b +c,d @@` block: the header line and the raw body lines (markers kept). */
export const Hunk = z.object({
  header: z.string(),
  lines: z.array(z.string()),
});
export type Hunk = z.infer<typeof Hunk>;

/** One changed file: its workspace-relative path, coarse status, and hunks. */
export const FileDiff = z.object({
  path: z.string(),
  status: FileStatus,
  hunks: z.array(Hunk),
});
export type FileDiff = z.infer<typeof FileDiff>;

/** A full review payload from one snapshot source. */
export const Diff = z.object({
  source: z.enum(["clonefile", "shadow"]),
  files: z.array(FileDiff),
  /** Set when this is a direct subagent's unadopted branch surfaced in its
   * SPAWNER's rail: the subagent session to adopt (POST /sessions/:id/adopt).
   * These entries are review-only for the spawner — apply/revert don't take them. */
  subagentId: z.string().optional(),
  /** Display label for a grouped diff section (e.g. `<subagent title> (unadopted)`). */
  label: z.string().optional(),
});
export type Diff = z.infer<typeof Diff>;

// ---- Changes-API request bodies --------------------------------------------

/**
 * Apply reviewed changes. For `clonefile`, `paths` are the original absolute paths to
 * copy back over. For `shadow`, `paths` selects which changed files to materialize
 * into the origin; covering every changed path also seals the change.
 */
export const ChangesApplyBody = z.object({
  source: z.enum(["clonefile", "shadow"]),
  paths: z.array(z.string()),
});
export type ChangesApplyBody = z.infer<typeof ChangesApplyBody>;

/** Revert a snapshot-workspace session's changes. Non-empty `paths` reverts only those
 * paths back to the session base; empty/absent `paths` reverts the whole change. */
export const ChangesRevertBody = z.object({
  paths: z.array(z.string()).optional(),
});
export type ChangesRevertBody = z.infer<typeof ChangesRevertBody>;

/**
 * Parse a unified/`--git` diff into `FileDiff[]`. Shared by shadow.ts (`git diff`)
 * and clonefile.ts (`git diff --no-index`) so both sources produce byte-identical
 * structure. Pure and dependency-free.
 *
 * `stripPrefix` drops a leading path segment from the `a/`,`b/` names: `git
 * --no-index` reports absolute paths, so callers pass the snapshot/original roots
 * to recover a clean relative path. shadow already emits repo-relative names, so it
 * passes nothing.
 */
export function parseGitDiff(text: string, stripPrefix?: (p: string) => string): FileDiff[] {
  const files: FileDiff[] = [];
  const lines = text.split("\n");
  let cur: FileDiff | null = null;
  let hunk: Hunk | null = null;

  const flushHunk = () => {
    if (cur && hunk) cur.hunks.push(hunk);
    hunk = null;
  };
  const flushFile = () => {
    flushHunk();
    if (cur) files.push(cur);
    cur = null;
  };

  for (const line of lines) {
    if (line.startsWith("diff --git ")) {
      flushFile();
      // "diff --git a/foo b/foo" — take the b-side as the canonical path.
      const parts = line.split(" ");
      const bRaw = parts[parts.length - 1].replace(/^b\//, "");
      const path = stripPrefix ? stripPrefix(bRaw) : bRaw;
      cur = { path, status: "modified", hunks: [] };
      continue;
    }
    if (!cur) continue;

    if (line.startsWith("new file mode")) cur.status = "added";
    else if (line.startsWith("deleted file mode")) cur.status = "deleted";
    else if (line.startsWith("rename from") || line.startsWith("rename to")) {
      // A pure rename with no content change carries no hunks; leave as modified.
    } else if (line.startsWith("--- ") || line.startsWith("+++ ")) {
      // File headers — the b-side name is already captured; skip.
    } else if (line.startsWith("@@")) {
      flushHunk();
      hunk = { header: line, lines: [] };
    } else if (hunk && (line.startsWith(" ") || line.startsWith("+") || line.startsWith("-"))) {
      hunk.lines.push(line);
    } else if (hunk && line === "\\ No newline at end of file") {
      hunk.lines.push(line);
    }
  }
  flushFile();
  return files;
}
