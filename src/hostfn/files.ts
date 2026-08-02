/**
 * The file verbs — `view`, `patch`, `write` — and the per-session snapshot store
 * that is what lets an empty tag mean anything at all.
 *
 * WHY THIS EXISTS. `hostfn/patch.ts` is pure: strings in, strings out. It can
 * decide whether a patch rebases or conflicts, but only if someone hands it the
 * text the agent actually READ. That text is the one piece of state the engine
 * cannot compute, and holding it is this module's job. Everything else here is a
 * thin IO shell: read the file, call the engine, write what it returns.
 *
 * THE INVARIANT THIS HOLDS: **`[path#]` — the empty tag — always means the exact
 * bytes this session last saw at that path, and a patch is refused outright when
 * there are no such bytes on record.** That single rule is what makes the normal
 * case (empty tag) exactly as safe as an explicit one, and it is why:
 *
 *   - `view()` RECORDS the text it renders, keyed by the RESOLVED path — so
 *     `view("m.ts")` and a later `[./m.ts#]` are one record, not two.
 *   - `patch()` records what it just wrote, so the TAG it echoes is live: a second
 *     patch chains onto it without viewing again (spec §6). This is the whole
 *     reason the tag is echoed rather than merely computed.
 *   - `write()` records too, for the same reason — writing a file is a way of
 *     seeing it.
 *   - A section naming a file this session never viewed is REFUSED, not applied
 *     against whatever the file happens to be now. Applying it is precisely the
 *     silent clobber the tag exists to prevent (see `patch.ts`, `ApplyOptions`).
 *
 * Keeping the TEXT and not just its hash is what makes a stale patch *recoverable*
 * rather than merely *detectable*: when the other writer stayed out of the patched
 * lines, the engine rebases and both edits land. Without the base text the only
 * honest answer to a tag mismatch is "re-view and try again", and every concurrent
 * pair of edits costs a wasted round — which, with subagents sharing one checkout
 * (spec §7), is the common case rather than the exotic one.
 *
 * There is no `read()` and no `edit()` (spec §17). One editing idiom. Raw content
 * comes from `readFile` or `bash` — a program has the full runtime and
 * does not need a host function for it.
 *
 * The workspace is the ORIGIN for relative paths, never a boundary: an absolute
 * path anywhere the user can reach resolves unchanged, matching `bash`, which runs
 * unconfined in the same places. Nothing here is a security mechanism (spec §2.2);
 * `paths.confine` guards the *server's* own path construction, not the agent's.
 *
 * Written fresh; the IO shape follows `src/tools/patch_file.ts`, whose snapshot
 * store and LRU bounds are ported reasoning. Deltas from it are marked `NOTE:`.
 */

import type { Stats } from "node:fs";
import { mkdir, readFile, stat as statFile, writeFile } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { BadRequestError, NotFoundError, PatchError } from "../errors.ts";
import type { HostFns, TurnCtx } from "../types.ts";
import { applyPatch, groupByFile, parsePatch, renderNumbered, tagOf, toLines } from "./patch.ts";

// ---------------------------------------------------------------------------
// The snapshot store
// ---------------------------------------------------------------------------

/**
 * What one session last saw at each path. Bounded, because a long session touches
 * many files and only recently-seen ones are plausible patch targets: the oldest
 * entry is dropped rather than growing without limit. A dropped snapshot costs a
 * re-view, never a wrong edit — every path out of this store either produces the
 * base text or refuses the patch.
 */
export const MAX_SNAPSHOTS_PER_SESSION = 64;

/**
 * …and a bound on sessions too. The server runs for weeks, so keying by session
 * without a cap is a slow leak; evicting the least recently active session costs
 * at worst a re-view in a session nobody has touched in a long time.
 */
export const MAX_SESSIONS = 32;

/**
 * Per-session memory of viewed text, LRU-bounded on both axes.
 *
 * Memory-only and deliberately so: it holds the *reading* half of an editing
 * idiom, not durable state. A server restart loses it, and the correct consequence
 * is that the next patch is refused with "call view() first" — which is a wasted
 * round, not a wrong edit.
 *
 * Scoping is per session, not per lineage: a subagent is its own session and must
 * view a file itself before patching it. That is not friction to remove — it is
 * the point. Two agents sharing a checkout must each anchor to what THEY read, or
 * the hash anchoring stops distinguishing "I saw this" from "someone told me".
 */
export class SnapshotStore {
  readonly #maxSessions: number;
  readonly #maxPerSession: number;
  /** sessionId → (absolute path → text). Map order is least-recently-used first. */
  readonly #bySession = new Map<string, Map<string, string>>();

  constructor(opts: { maxSessions?: number; maxPerSession?: number } = {}) {
    this.#maxSessions = opts.maxSessions ?? MAX_SESSIONS;
    this.#maxPerSession = opts.maxPerSession ?? MAX_SNAPSHOTS_PER_SESSION;
  }

  /** Remember the text a session just saw. Call after view, patch and write. */
  record(sessionId: string, absPath: string, text: string): void {
    const files = this.#touch(sessionId);
    files.delete(absPath); // re-insert so iteration order tracks recency
    files.set(absPath, text);
    while (files.size > this.#maxPerSession) {
      files.delete(files.keys().next().value as string);
    }
  }

  /** The text this session last saw at `absPath`, if it is still held. */
  get(sessionId: string, absPath: string): string | undefined {
    return this.#bySession.get(sessionId)?.get(absPath);
  }

  /** Live path count for a session — the eviction tests read it. */
  size(sessionId: string): number {
    return this.#bySession.get(sessionId)?.size ?? 0;
  }

  /** Forget everything a session saw. Not called in the turn path; here for tests. */
  clear(sessionId: string): void {
    this.#bySession.delete(sessionId);
  }

  #touch(sessionId: string): Map<string, string> {
    const existing = this.#bySession.get(sessionId);
    if (existing) {
      // Re-insert on every touch so Map order is least-recently-used first.
      this.#bySession.delete(sessionId);
      this.#bySession.set(sessionId, existing);
      return existing;
    }
    const created = new Map<string, string>();
    this.#bySession.set(sessionId, created);
    while (this.#bySession.size > this.#maxSessions) {
      this.#bySession.delete(this.#bySession.keys().next().value as string);
    }
    return created;
  }
}

/**
 * The process-wide store the turn runner gets by default.
 *
 * NOTE on dependency injection (plan §0): a snapshot's lifetime is the SESSION,
 * which spans turns — "the version I just viewed" has to survive the round that
 * viewed it — while `createFileHostFns` is called per turn from a `TurnCtx`. The
 * context frozen in T-1 carries no place to hang a store, so the default lives
 * here and every entry is keyed by session id. Tests inject their own store and
 * therefore never see another test's snapshots; nothing else in the tree reads
 * this binding.
 */
export const sessionSnapshots = new SnapshotStore();

/**
 * Which paths a session's own programs WROTE, keyed by session id.
 *
 * Delegated reports said `Changed files: not reported` on every fan-out — the field exists,
 * the code that fills it was never wired, and a delegator persona pointed out that for a
 * review the file→agent mapping IS the review. Git cannot answer it: subagents share their
 * spawner's checkout by design, so `git diff` at the end reports the union of every
 * concurrent sibling's work and attributes it to whoever asked last.
 *
 * The write verbs know exactly what they wrote, so this records it at the source. Keyed the
 * same way as the snapshots above and, like them, module-level so nothing has to be threaded
 * through the bridge.
 */
const sessionWrites = new Map<string, Set<string>>();

/** Cap per session: a report lists files, and a runaway loop must not grow this forever. */
const MAX_TRACKED_WRITES = 200;

function recordWrite(sessionId: string, path: string): void {
  const set = sessionWrites.get(sessionId) ?? new Set<string>();
  if (set.size < MAX_TRACKED_WRITES) set.add(path);
  sessionWrites.set(sessionId, set);
}

/**
 * The paths this session wrote, in write order, and FORGET them.
 *
 * Read-and-clear because the only caller is a report built once, and a store that only ever
 * grows in a process that runs for weeks is a leak with extra steps.
 */
export function takeSessionWrites(sessionId: string): string[] {
  const set = sessionWrites.get(sessionId);
  if (!set) return [];
  sessionWrites.delete(sessionId);
  return [...set];
}

// ---------------------------------------------------------------------------
// The verbs
// ---------------------------------------------------------------------------

/**
 * Everything the file verbs need from a turn. Deliberately narrower than
 * `TurnCtx` — a `Pick`, so `TurnCtx` satisfies it — because these three functions
 * touch no database, no bus and no LLM, and a test should not have to fabricate
 * one to edit a file in a temp directory.
 */
export type FileCtx = Pick<TurnCtx, "workspace" | "sessionId" | "reads">;

/** The three bridged file functions, as `HostFns` declares them. */
export type FileHostFns = Pick<HostFns, "view" | "patch" | "write">;

/**
 * NOTE: not in the port. A view is rendered into the model's context in full, so an
 * unbounded one is a context overflow that ends the turn with an error nobody can
 * act on. Refusing is strictly better than truncating: a truncated listing would
 * still carry line numbers, and the model cannot tell that lines past the cut exist
 * — it would write anchors against a version it never saw. So this bound REFUSES
 * and names the tool that reads a slice instead.
 */
export const MAX_VIEW_BYTES = 2 * 1024 * 1024;

export interface FileHostFnsOptions {
  /** The snapshot store. Defaults to the process-wide one; tests inject. */
  snapshots?: SnapshotStore;
}

/**
 * Build the file verbs for one turn.
 *
 * All three are string-in/string-out because the bridge wire is (protocol.ts), and
 * for these three the text IS the payload — nothing is JSON-wrapped.
 */
export function createFileHostFns(ctx: FileCtx, opts: FileHostFnsOptions = {}): FileHostFns {
  const snapshots = opts.snapshots ?? sessionSnapshots;
  /** The workspace is the origin for relative paths, not a boundary. */
  const abs = (path: string) => resolve(ctx.workspace, path);

  /**
   * `[path#TAG]` plus numbered `N:text` lines — and the record that makes a later
   * `[path#]` resolvable. Rendering without recording would be a lie: the model
   * would be handed a tag naming a version nothing can produce again.
   */
  async function view(path: string): Promise<string> {
    const p = requirePath(path, "view");
    const full = abs(p);

    let stat: Stats;
    try {
      stat = await statFile(full);
    } catch (err) {
      throw viewReadError(p, full, err);
    }
    if (stat.isDirectory()) {
      throw new BadRequestError(
        `cannot view ${p}: it is a directory, not a file. List it with ` +
          `bash("ls -la ${shellQuote(p)}") and view one of the files inside it.`,
      );
    }
    if (stat.size > MAX_VIEW_BYTES) {
      throw new BadRequestError(
        `cannot view ${p}: it is ${stat.size} bytes, over the ${MAX_VIEW_BYTES}-byte ` +
          `view limit, and rendering it would overflow the context window. Read the ` +
          `part you need with bash (rg -n PATTERN ${shellQuote(p)}, or ` +
          `sed -n '1,200p' ${shellQuote(p)}); patch() needs a view() of the file to ` +
          `anchor to, so edit a smaller file or rewrite this one with write().`,
      );
    }

    let text: string;
    try {
      text = await readFile(full, "utf8");
    } catch (err) {
      throw viewReadError(p, full, err);
    }
    // Decoding is lossy, so a binary file arrives as replacement characters and
    // writing it back would destroy it. Refuse before it is on record.
    if (text.includes("\u0000")) {
      throw new BadRequestError(
        `cannot view ${p}: it contains NUL bytes, so it is not a text file — ` +
          `viewing it would decode it lossily and patching it would corrupt it. ` +
          `Inspect it with bash instead (file ${shellQuote(p)}).`,
      );
    }

    // Keyed by the RESOLVED path: "m.ts" and "./m.ts" are one file and must be one
    // record, and the same relative path means different files in different
    // workspaces.
    snapshots.record(ctx.sessionId, full, text);
    // The read trail behind the directory-triggered tag hints (`history/stats.ts`).
    // Appended, never consulted here — the runner reads it at round end.
    ctx.reads?.push(full);

    const rendered = renderNumbered(p, text);
    if (text.length === 0) {
      return `${rendered.trimEnd()}\n(this file is empty — use INS.HEAD: to put the ` +
        `first lines in, or write() to replace it wholesale)`;
    }
    return rendered;
  }

  /**
   * Apply hash-anchored edits and echo each file's NEW tag.
   *
   * Order is load-bearing for the all-or-none rule (spec §6): parse, read every
   * file, let the engine validate/rebase/assemble every file, and only then write.
   * A patch that fails on its third file has written nothing.
   */
  async function patch(input: string): Promise<string> {
    const ops = parsePatch(input);
    const groups = groupByFile(ops);

    const full = new Map<string, string>();
    const current = new Map<string, string>();
    const base = new Map<string, string>();
    /** absolute path → the section path that claimed it, for the aliasing check. */
    const claimed = new Map<string, string>();

    for (const g of groups) {
      const p = abs(g.path);
      // `groupByFile` merges by the literal string, so "a.ts" and "./a.ts" would be
      // two groups over one file — the second write computed from the pre-patch text
      // would silently discard the first. Refuse instead.
      const other = claimed.get(p);
      if (other !== undefined) {
        throw new PatchError(
          `"${other}" and "${g.path}" name the same file (${p}) in one patch, so ` +
            `the second set of operations would be written against the version from ` +
            `before the first — silently discarding it. Nothing was written. Put all ` +
            `of that file's operations under a single "[${other}#]" section.`,
        );
      }
      claimed.set(p, g.path);
      full.set(g.path, p);

      let text: string;
      try {
        text = await readFile(p, "utf8");
      } catch (err) {
        throw patchReadError(g.path, p, err);
      }
      current.set(g.path, text);

      // Absent = never viewed. The engine refuses that section by name; leaving the
      // entry out is how it is told (patch.ts, `ApplyOptions`).
      const snapshot = snapshots.get(ctx.sessionId, p);
      if (snapshot !== undefined) base.set(g.path, snapshot);
    }

    // Throws `PatchError` — stale tag, conflict, bad anchor — before returning
    // anything, so nothing below runs on a half-decided patch.
    const next = applyPatch(current, ops, { base });

    const written: string[] = [];
    const out: string[] = [];
    for (const g of groups) {
      const p = full.get(g.path)!;
      const text = next.get(g.path)!;
      try {
        await writeFile(p, text);
      } catch (err) {
        // Every file was decided before any was written, so this is a filesystem
        // failure (permissions, a full disk), not a patch decision. Say exactly how
        // far it got — the alternative is the model re-applying edits that landed.
        throw new PatchError(
          `cannot write ${g.path}: ${errText(err)}. ${
            written.length === 0
              ? "Nothing was written."
              : `Already written and NOT rolled back: ${written.join(", ")} — ` +
                `re-view those before editing them again.`
          } The remaining files in this patch were not written.`,
        );
      }
      // What this session last saw at the path is now what it just wrote, so the
      // echoed tag is live: a follow-up patch may anchor to it, or use "[path#]",
      // without viewing again (spec §6).
      snapshots.record(ctx.sessionId, p, text);
      recordWrite(ctx.sessionId, g.path);
      written.push(g.path);
      out.push(
        `[${g.path}#${tagOf(text)}] patched — ${plural(g.ops.length, "operation")}, ` +
          `now ${plural(toLines(text).length, "line")}`,
      );
    }
    return out.join("\n");
  }

  /**
   * New files and wholesale rewrites. Parent directories are created, because the
   * alternative is a program that has to `bash("mkdir -p")` before every new file.
   *
   * NOTE: the port's `write_file` recorded nothing. Recording is what lets a
   * freshly written file be patched with `[path#]` in the same round — the program
   * knows exactly what it just wrote, so requiring a view() of its own output is
   * pure ceremony.
   */
  async function write(path: string, content: string): Promise<string> {
    const p = requirePath(path, "write");
    const full = abs(p);
    if (typeof content !== "string") {
      throw new BadRequestError(
        `cannot write ${p}: content must be a string. Serialize objects yourself ` +
          `(JSON.stringify(value, null, 2)) so the file holds exactly what you meant.`,
      );
    }
    try {
      const dir = dirname(full);
      if (dir && dir !== full) await mkdir(dir, { recursive: true });
      await writeFile(full, content);
    } catch (err) {
      throw new BadRequestError(`cannot write ${p}: ${errText(err)}`);
    }
    snapshots.record(ctx.sessionId, full, content);
    recordWrite(ctx.sessionId, p);
    const bytes = new TextEncoder().encode(content).length;
    return `[${p}#${tagOf(content)}] wrote ${plural(toLines(content).length, "line")} ` +
      `(${plural(bytes, "byte")})`;
  }

  return { view, patch, write };
}

// ---------------------------------------------------------------------------
// Error text — a product surface (spec §6): what failed, the state, the move
// ---------------------------------------------------------------------------

function requirePath(path: string, verb: string): string {
  if (typeof path !== "string" || path.trim() === "") {
    throw new BadRequestError(
      `${verb}() needs a path — it was called with ${JSON.stringify(path)}. Pass a ` +
        `path relative to the workspace, or an absolute one.`,
    );
  }
  return path;
}

function viewReadError(path: string, full: string, err: unknown): Error {
  if ((err as NodeJS.ErrnoException)?.code === "ENOENT") {
    return new NotFoundError(
      `cannot view ${path}: no such file (looked at ${full}). Relative paths ` +
        `resolve against the workspace — check the path with ` +
        `bash("ls ${shellQuote(dirname(path) || ".")}"), or create it with ` +
        `write("${path}", …).`,
    );
  }
  return new BadRequestError(`cannot view ${path}: ${errText(err)}`);
}

function patchReadError(path: string, full: string, err: unknown): PatchError {
  if ((err as NodeJS.ErrnoException)?.code === "ENOENT") {
    return new PatchError(
      `cannot patch ${path}: no such file (looked at ${full}). patch() edits a file ` +
        `that exists — create it with write("${path}", …) instead. Nothing was ` +
        `written; a patch applies to all its files or none.`,
    );
  }
  return new PatchError(
    `cannot patch ${path}: ${errText(err)}. Nothing was written; a patch applies to ` +
      `all its files or none.`,
  );
}

function errText(err: unknown): string {
  return (err as Error)?.message ?? String(err);
}

/** Single-quote a path for the shell hints above, so a space or `$` is inert. */
function shellQuote(s: string): string {
  return /^[\w./-]+$/.test(s) ? s : `'${s.replace(/'/g, `'\\''`)}'`;
}

function plural(n: number, noun: string): string {
  return `${n} ${noun}${n === 1 ? "" : "s"}`;
}
