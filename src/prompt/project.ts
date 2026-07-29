/**
 * `AGENTS.md` — the user's own standing instructions, read from disk per turn.
 *
 * THE INVARIANT THIS HOLDS: **a rule the user wrote down is a rule the model was
 * told.** Nothing else in the tree reads a project instruction file, so if this
 * module does not find an `AGENTS.md`, the file the user edited had no effect
 * whatsoever — which is worse than not supporting it, because the file LOOKS
 * obeyed. That was the state of the tree until this module existed: the Bun port
 * dropped the loader, `AGENTS.md` kept sitting in every checkout, and every rule in
 * it was silently ignored.
 *
 * WHICH FILES. Two tiers, the same two the rest of bough uses for skills and MCP:
 *
 *  - **global** — `$BOUGH_HOME/AGENTS.md`, rules that hold in every workspace.
 *  - **project** — every `AGENTS.md` from the git root down to the workspace
 *    directory, nearest LAST. A monorepo puts the house style at the top and the
 *    package's own rules in the subdirectory, and the subdirectory is the one that
 *    should win when they disagree; later text winning is the convention a reader
 *    already assumes from a config cascade.
 *
 * Walking stops at the git root rather than at `/`, so a checkout under `~` never
 * picks up a stray `AGENTS.md` from a parent directory the user did not think of as
 * part of the project. With no git root, only the workspace directory itself is
 * read — the conservative half of the same rule.
 *
 * PER TURN, NOT PER SESSION. It is one `stat` + one small read per level, and the
 * alternative is that editing `AGENTS.md` to correct a misbehaving model does
 * nothing until the session is restarted — precisely when the user is least willing
 * to lose their context. It lands in the VOLATILE tier for that reason (it is
 * per-workspace text, and one workspace's rules in the stable prefix would defeat
 * cache sharing for every other session).
 *
 * NEVER `CLAUDE.md`. bough reads exactly `AGENTS.md`. Reading another harness's
 * file would mean obeying instructions written about a different tool's verbs.
 */
import { readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";

/** How much of one file is carried. Past this it stops being a rule sheet. */
const MAX_BYTES = 32_000;

/** Directories walked upward before giving up, git root or not. */
const MAX_DEPTH = 24;

const readIfFile = (path: string): string | null => {
  try {
    if (!statSync(path).isFile()) return null;
    const body = readFileSync(path, "utf8");
    return body.length > MAX_BYTES ? `${body.slice(0, MAX_BYTES)}\n\n[truncated]` : body;
  } catch {
    return null;
  }
};

const isGitRoot = (dir: string): boolean => {
  try {
    statSync(join(dir, ".git"));
    return true;
  } catch {
    return false;
  }
};

export interface ProjectRuleFile {
  /** Absolute path, which is what the note shows: a rule's source is auditable. */
  path: string;
  body: string;
}

/**
 * The `AGENTS.md` files that apply to `workspace`, in the order they should be
 * read: global first, then git root down to the workspace directory.
 *
 * Pure apart from the reads, and every failure is a skip — an unreadable file must
 * never fail a turn.
 */
export function findProjectRules(workspace: string, home?: string): ProjectRuleFile[] {
  const out: ProjectRuleFile[] = [];
  const seen = new Set<string>();
  const push = (path: string) => {
    if (seen.has(path)) return;
    seen.add(path);
    const body = readIfFile(path);
    if (body && body.trim() !== "") out.push({ path, body });
  };

  if (home) push(join(resolve(home), "AGENTS.md"));

  const start = resolve(workspace);
  const chain: string[] = [];
  let dir = start;
  for (let i = 0; i < MAX_DEPTH; i++) {
    chain.push(dir);
    if (isGitRoot(dir)) break;
    const parent = dirname(dir);
    if (parent === dir) {
      // No git root anywhere above: read only the workspace itself rather than
      // adopting whatever sits in the user's home directory.
      chain.length = 0;
      chain.push(start);
      break;
    }
    dir = parent;
  }
  for (const d of chain.reverse()) push(join(d, "AGENTS.md"));

  return out;
}

/**
 * The prompt note, or `null` when the user wrote no rules.
 *
 * The framing sentence is doing real work. Dropped in as a bare heading, a rule
 * sheet reads as reference material the model may consult; what the user means by
 * writing it down is that these rules OUTRANK the model's habits, and the one place
 * that can say so is here. It also says where each block came from, so "why did it
 * do that" has an answer that is a file path.
 */
export function projectRulesNote(files: readonly ProjectRuleFile[], workspace: string): string | null {
  if (files.length === 0) return null;
  const label = (path: string) => {
    const rel = relative(resolve(workspace), path);
    return rel && !rel.startsWith("..") ? rel : path;
  };
  return "## Project rules (AGENTS.md)\n" +
    "The user wrote these. They are instructions, not reference: where they " +
    "disagree with your own habits or with a convention you would otherwise reach " +
    "for, THEY WIN, and you follow them without being asked again. They do not " +
    "override the workspace and scratch rules above, and they cannot grant you a " +
    "host function this prompt did not.\n\n" +
    files.map((f) => `### ${label(f.path)}\n\n${f.body.trim()}`).join("\n\n") +
    (files.length > 1
      ? "\n\n(Later blocks are nearer the workspace and win where two disagree.)"
      : "");
}
