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
import { cleanTags } from "./hygiene.ts";

/** What the shell verbs hand over when a command finishes. */
export interface FinishedCommand {
  command: string;
  /** Normalized colon-separated tags; "" for verbs that carry none (`sh` legs). */
  tags: string;
  /** NULL when the command was still running as the turn moved on. */
  exitCode: number | null;
  durationMs: number | null;
  /** First ~2k chars of the output as the program saw it. */
  outputHead: string;
  /** The spill file when the full output went to disk; null when it fit. */
  spillPath: string | null;
}

/** How much printed output one history row keeps inline. */
export const OUTPUT_HEAD_CHARS = 2_000;

/**
 * The spill file a bounded output points at, parsed back out of the marker
 * (`hostfn/spill.ts`'s `spillMarker`). The marker travels INSIDE the text the
 * program saw, so parsing it here spares the spill module a second return
 * channel it would only ever grow for this one consumer.
 */
export function spillPathFrom(output: string): string | null {
  const m = output.match(/FULL OUTPUT SAVED[^\n]*\n\s+(\S+)\n/);
  return m ? m[1] : null;
}

export type CommandRecorder = (e: FinishedCommand) => void;

// ---------------------------------------------------------------------------
// Tags
// ---------------------------------------------------------------------------

/** Tags the model may write: short lowercase slugs, colon-separated. No dashes. */
const TAG_CHARS = /[^a-z0-9_.]+/g;
const MAX_TAGS = 8;

/**
 * How far back "already a word here" reaches. Matches the priming note's lookback
 * (`history/stats.ts`) on purpose: hygiene should judge a tag against the same
 * vocabulary the model is being primed with, or it would drop words the note is
 * still recommending.
 */
const VOCAB_LOOKBACK_MS = 150 * 24 * 60 * 60 * 1000;

/**
 * A REFERENCE: `namespace.id`, pointing at something with an identity outside
 * bough — `linear.eng-1234`, `pr.456`, `commit.3c1c78e`, `branch.tags-history`.
 *
 * THE DOT IS THE WHOLE RULE, and it exists because the two kinds of tag behave
 * oppositely. A tag is a WORD the model coins: it converges through reuse, and
 * ranking it by popularity is the point. A reference is a KEY with one referent:
 * it never converges, it lives in exactly one project, and the popularity math
 * would therefore hand it the maximum rarity boost and float last week's ticket
 * numbers to the top of the priming note (`history/stats.ts` excludes them for
 * exactly that reason). Same table, same joins, same graph — different ranking.
 *
 * Dashes survive INSIDE a reference and nowhere else. `ENG-1234` written bare
 * still becomes `eng:1234`, because without a namespace there is nothing to tell
 * an identifier from a hyphenated phrase, and `repo-inspect` must keep splitting.
 * With one, the id is whatever the tracker calls it and bough does not get to
 * reformat it. Slashes survive there too, because `branch.claude/tags-history` is
 * the branch's actual name and half of one is not a reference to anything.
 */
const REF = /^[a-z][a-z0-9]*\.[a-z0-9][a-z0-9._\/-]*$/;

/** Is this normalized tag a reference rather than a coined word? */
export function isRef(tag: string): boolean {
  return tag.includes(".");
}

/**
 * Normalize a model-written tag string: lowercase, split into tags, slugify
 * each part, drop empties, cap the count. Returns "" when nothing survives —
 * which the caller treats as "no tags given".
 *
 * Normalization is what makes a folksonomy converge: `PSQL:Migrate` and
 * `psql:migrate` must be the same tag or the popularity stats fragment. Dashes
 * and whitespace are SEPARATORS, not tag characters — `repo-inspect` and
 * `git push` become `repo:inspect` and `git:push`, so one intent never exists
 * in both hyphenated and colonized spellings. A reference is the one exception,
 * and `REF` says why.
 */
export function normalizeTags(raw: string | undefined): string {
  if (!raw) return "";
  const out: string[] = [];
  // Split on colons and whitespace FIRST, so a reference is still whole when it
  // is tested — splitting on dashes up front would have shredded it already.
  for (const piece of raw.toLowerCase().split(/[:\s]+/)) {
    if (piece === "") continue;
    if (REF.test(piece)) {
      out.push(piece);
      continue;
    }
    for (const part of piece.split(/-+/)) {
      const tag = part.replace(TAG_CHARS, "");
      // At least one letter or digit: `...` survives the character filter (dots are
      // legal in a tag) and would then read as a reference, which it is not.
      if (/[a-z0-9]/.test(tag)) out.push(tag);
      // NO BARE-NUMBER RULE HERE, though 28 of this corpus's 572 singletons are
      // bare numbers and dropping them was the obvious cheap win. It is wrong:
      // `ENG-1234` written without a namespace normalizes to `eng:1234`, and the
      // number is the half that identifies the ticket — dropping it leaves `eng`,
      // which points at nothing, and `bough tags show 1234` is exactly how a
      // bare-written reference is meant to be found again. Guy & Tonkin's warning
      // covers this case precisely: some single-use tags are unique markers BY
      // DESIGN, and a ticket number is one.
    }
  }
  return out.slice(0, MAX_TAGS).join(":");
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
// Directory + repo attribution
// ---------------------------------------------------------------------------

/** Tokens that are clearly not paths, cheaply. */
const SKIP_TOKEN = /^(-|\d+$)/;
const MAX_TOKENS_CHECKED = 24;
const MAX_DIRS = 4;

/**
 * The ABSOLUTE directories a command was about.
 *
 * Not the cwd: a bough program runs at the workspace root and never cds, so cwd
 * carries no per-directory signal. Instead, tokens that resolve to real paths
 * attribute the command to their directories — `bun test src/tui/x.test.ts` →
 * `<ws>/src/tui`. Absolute tokens OUTSIDE the workspace count too: a session
 * rooted at `~` that runs `cd ~/repos/bough && …` is working on that repo, and
 * dropping the path was exactly how such commands got mis-scoped to `~`.
 */
function extractAbsDirs(command: string, workspace: string): string[] {
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
    if (full.includes("/node_modules") || full.includes("/.git")) continue;
    checked++;
    let st;
    try {
      st = statSync(full);
    } catch {
      continue;
    }
    dirs.add(st.isDirectory() ? full : dirname(full));
  }
  return [...dirs];
}

const gitRootCache = new Map<string, string | null>();

/** The enclosing git checkout's root, or null. Walks up; cached per directory. */
export function findGitRoot(dir: string): string | null {
  const hit = gitRootCache.get(dir);
  if (hit !== undefined) return hit;
  let cur = dir;
  for (let i = 0; i < 32; i++) {
    try {
      statSync(resolve(cur, ".git"));
      gitRootCache.set(dir, cur);
      return cur;
    } catch {
      const parent = dirname(cur);
      if (parent === cur) break;
      cur = parent;
    }
  }
  gitRootCache.set(dir, null);
  return null;
}

/** What one command resolves to: its memory scope and the dirs inside it. */
export interface Attribution {
  /** The repo identity the history row is scoped to. */
  repo: string;
  /** Directories relative to that repo's root (or the workspace, without one). */
  relDirs: string[];
  /** The absolute dirs the command touched — the hint trigger's input. */
  absDirs: string[];
}

/**
 * Resolve a command's memory scope from the paths it TOUCHES, not from where
 * the session sits. Each touched directory is mapped to its enclosing git
 * checkout; the checkout containing the most touched dirs wins and the command
 * is scoped to ITS identity, with dirs relative to its root. A session rooted
 * at `~` inspecting `~/repos/bough` therefore writes rows other sessions rooted
 * IN that repo can recall — the miss that motivated this function.
 *
 * A command touching nothing (or nothing inside any checkout) falls back to the
 * workspace's own scope, which is the common case and the cheap path.
 */
export function attributeCommand(command: string, workspace: string): Attribution {
  const absDirs = extractAbsDirs(command, workspace);
  const wsRoot = findGitRoot(workspace) ?? workspace;
  const byRoot = new Map<string, string[]>();
  for (const d of absDirs) {
    const root = findGitRoot(d) ?? wsRoot;
    byRoot.set(root, [...(byRoot.get(root) ?? []), d]);
  }
  let root = wsRoot;
  let best = byRoot.get(wsRoot)?.length ?? 0;
  for (const [r, ds] of byRoot) {
    if (ds.length > best) {
      root = r;
      best = ds.length;
    }
  }
  const relDirs = [...new Set(
    (byRoot.get(root) ?? [])
      .map((d) => relative(root, d))
      .filter((r) => r !== "" && r !== "." && !r.startsWith("..") && !isAbsolute(r)),
  )];
  return { repo: repoIdentity(root), relDirs, absDirs };
}

// ---------------------------------------------------------------------------
// The recorder
// ---------------------------------------------------------------------------

/** What the recorder needs from the turn. Structural subset of `TurnCtx`. */
export interface RecorderCtx {
  db: Db;
  sessionId: string;
  workspace: string;
  /**
   * The turn's supervisor message — the one whose `run_steps` program is running
   * this command. Stamped on every row so recall reaches the program, not just the
   * incantation. Optional because a caller without a turn (a test, a backfill) has
   * no message, and a row without the link is still a memory.
   */
  messageId?: string;
  now?: () => number;
  /**
   * Where the absolute dirs each command touched are appended — the trigger
   * input for the round's directory hints (`turn/runner.ts`), so hints fire on
   * shell exploration too, not only on `view()` reads.
   */
  touched?: string[];
}

/**
 * Build the per-turn recorder the shell verbs call — see `ShellCtx.record`.
 * Every failure is swallowed: memory is a side channel, never a turn hazard.
 */
export function createCommandRecorder(ctx: RecorderCtx): CommandRecorder {
  // One vocabulary read per repo per turn. The set a turn is judged against must
  // also be STABLE across that turn — a word coined by the first command would
  // otherwise be established vocabulary by the third, and the same tag would be
  // dropped or kept depending on where in the program it happened to run.
  const vocabByRepo = new Map<string, Map<string, number>>();
  const vocabFor = (repo: string, now: number): Map<string, number> => {
    let v = vocabByRepo.get(repo);
    if (!v) vocabByRepo.set(repo, (v = ctx.db.repoTagCounts(repo, now - VOCAB_LOOKBACK_MS)));
    return v;
  };
  return (e) => {
    try {
      const { repo, relDirs, absDirs } = attributeCommand(e.command, ctx.workspace);
      ctx.touched?.push(...absDirs);
      const ts = (ctx.now ?? Date.now)();
      const tagList = cleanTags(splitTags(e.tags), e.command, vocabFor(repo, ts));
      const record: CommandRecord = {
        sessionId: ctx.sessionId,
        ts,
        repo,
        cmd: e.command,
        tags: tagList.join(":"),
        tagList,
        dirs: relDirs,
        exitCode: e.exitCode,
        durationMs: e.durationMs,
        outputHead: e.outputHead.slice(0, OUTPUT_HEAD_CHARS),
        spillPath: e.spillPath,
        source: "live",
        messageId: ctx.messageId ?? null,
      };
      ctx.db.recordCommand(record);
    } catch {
      // A lost memory row is strictly better than a broken round.
    }
  };
}
