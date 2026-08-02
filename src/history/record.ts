/**
 * Recording finished shell commands into the tag-history memory (db/schema.sql:
 * `command_history` + junctions).
 *
 * The design bet: the model labels its own INTENT at generation time (`bash(cmd,
 * "psql:migrate")`), which is nearly free and far more accurate than post-hoc
 * clustering of command strings — the tag is the stable join key across sessions,
 * the exit code is the ground truth that weights it.
 *
 * Everything here is best-effort and MUST NEVER surface a failure into a turn: a
 * broken git checkout, a locked database, or a weird command string loses one
 * memory row, not the round. Same contract as `server/search.ts`'s indexing.
 */

import { spawnSync } from "node:child_process";
import { statSync } from "node:fs";
import { isAbsolute, dirname, relative, resolve } from "node:path";
import type { CommandRecord, Db } from "../types.ts";

/** What the shell verbs hand over when a command finishes. */
export interface FinishedCommand {
  command: string;
  /** Normalized colon-separated tags; "" for verbs that carry none (`sh` legs). */
  tags: string;
  /** NULL when the command was still running as the turn moved on. */
  exitCode: number | null;
  durationMs: number | null;
}

export type CommandRecorder = (e: FinishedCommand) => void;

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

/** Tags the model may write: short lowercase slugs, colon-separated. */
const TAG_CHARS = /[^a-z0-9_.-]+/g;
const MAX_TAGS = 8;

/**
 * Normalize a model-written tag string: lowercase, split on `:`, slugify each
 * part, drop empties, cap the count. Returns "" when nothing survives — which the
 * caller treats as "no tags given".
 *
 * Normalization is what makes a folksonomy converge: `PSQL:Migrate` and
 * `psql:migrate` must be the same tag or the popularity stats fragment.
 */
export function normalizeTags(raw: string | undefined): string {
  if (!raw) return "";
  return raw
    .toLowerCase()
    .split(":")
    .map((t) => t.trim().replace(/\s+/g, "-").replace(TAG_CHARS, ""))
    .filter((t) => t.length > 0)
    .slice(0, MAX_TAGS)
    .join(":");
}

/** The individual tags of a normalized string; [] for "". */
export function splitTags(tags: string): string[] {
  return tags === "" ? [] : [...new Set(tags.split(":"))];
}

// ---------------------------------------------------------------------------
// Repo identity
// ---------------------------------------------------------------------------

const repoCache = new Map<string, string>();

/**
 * The scope key for a workspace's command history: the git remote origin URL when
 * there is one, else the workspace root path.
 *
 * The URL rather than the path because paths are fragile — the same project moved,
 * re-cloned, or checked out on another machine should keep its tag profile. Cached
 * per workspace for the process lifetime; a mid-session `git remote set-url` is
 * not a case worth a subprocess per command.
 */
export function repoIdentity(workspace: string): string {
  const hit = repoCache.get(workspace);
  if (hit !== undefined) return hit;
  let repo = workspace;
  try {
    const r = spawnSync("git", ["-C", workspace, "config", "--get", "remote.origin.url"], {
      encoding: "utf8",
      timeout: 2_000,
    });
    const url = r.status === 0 ? r.stdout.trim() : "";
    if (url) repo = url;
  } catch {
    // No git, or a hostile checkout — the path is a fine identity.
  }
  repoCache.set(workspace, repo);
  return repo;
}

// ---------------------------------------------------------------------------
// Directory attribution
// ---------------------------------------------------------------------------

/** Tokens that are clearly not paths, cheaply. */
const SKIP_TOKEN = /^(-|\d+$)/;
const MAX_TOKENS_CHECKED = 24;
const MAX_DIRS = 4;

/**
 * The directories a command was ABOUT, workspace-relative.
 *
 * Not the cwd: a bough program runs at the workspace root and never cds, so cwd
 * carries no per-directory signal. Instead, tokens that resolve to real paths
 * under the workspace attribute the command to their directories — `bun test
 * src/tui/composer.test.ts` → `src/tui`. Cheap heuristic, wrong rarely, and when
 * it finds nothing the command is honestly "about the repo generally".
 */
export function extractDirs(command: string, workspace: string): string[] {
  const dirs = new Set<string>();
  const tokens = command.split(/[\s;|&<>()]+/).slice(0, 200);
  let checked = 0;
  for (const rawToken of tokens) {
    if (dirs.size >= MAX_DIRS || checked >= MAX_TOKENS_CHECKED) break;
    let tok = rawToken.replace(/^['"`]+|['"`,]+$/g, "");
    // `--output=path/x` and `FOO=path/x` both carry the path after `=`.
    const eq = tok.indexOf("=");
    if (eq >= 0) tok = tok.slice(eq + 1);
    // Line refs (`src/a.ts:12`) resolve after stripping the suffix.
    tok = tok.replace(/:\d+(:\d+)?$/, "");
    if (tok.length < 2 || SKIP_TOKEN.test(tok)) continue;
    // Only path-looking tokens are worth a stat: containing a separator, or a
    // dotted filename. Bare words (`git`, `push`) are commands, not paths.
    if (!tok.includes("/") && !/^[^./]+\.[^./]+$/.test(tok)) continue;
    if (tok.includes("://")) continue;
    const full = isAbsolute(tok) ? tok : resolve(workspace, tok);
    const rel = relative(workspace, full);
    if (rel === "" || rel.startsWith("..") || isAbsolute(rel)) continue;
    if (rel === ".git" || rel.startsWith(".git/") || rel.includes("node_modules")) continue;
    checked++;
    let st;
    try {
      st = statSync(full);
    } catch {
      continue;
    }
    const dir = st.isDirectory() ? rel : dirname(rel);
    if (dir !== "" && dir !== ".") dirs.add(dir);
  }
  return [...dirs];
}

// ---------------------------------------------------------------------------
// The recorder
// ---------------------------------------------------------------------------

/** What the recorder needs from the turn. Structural subset of `TurnCtx`. */
export interface RecorderCtx {
  db: Db;
  sessionId: string;
  workspace: string;
  now?: () => number;
}

/**
 * Build the per-turn recorder the shell verbs call — see `ShellCtx.record`.
 * Every failure is swallowed: memory is a side channel, never a turn hazard.
 */
export function createCommandRecorder(ctx: RecorderCtx): CommandRecorder {
  return (e) => {
    try {
      const record: CommandRecord = {
        sessionId: ctx.sessionId,
        ts: (ctx.now ?? Date.now)(),
        repo: repoIdentity(ctx.workspace),
        cmd: e.command,
        tags: e.tags,
        tagList: splitTags(e.tags),
        dirs: extractDirs(e.command, ctx.workspace),
        exitCode: e.exitCode,
        durationMs: e.durationMs,
        source: "live",
      };
      ctx.db.recordCommand(record);
    } catch {
      // A lost memory row is strictly better than a broken round.
    }
  };
}
