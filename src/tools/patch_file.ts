/**
 * The IO half of hash-anchored editing (hashedit.ts holds the pure logic):
 * `view` hands the model numbered lines under a content tag, `patch` applies ops
 * written against that tag.
 *
 * The snapshot store is what makes a stale patch recoverable rather than merely
 * detectable. Keeping the TEXT the session read — not just its hash — means a
 * patch anchored to an old version can be rebased onto the current file when the
 * other writer stayed out of its way, which is the common case when two subagents
 * work the same file. Without the base text the only honest answer to a tag
 * mismatch is "re-read and try again", and every concurrent pair of edits costs a
 * wasted round.
 */
import { z } from "zod/v4";
import { resolveInWorkspace, type ToolDef, type ToolRunCtx } from "./types.ts";
import {
  applyOps,
  checkOps,
  joinLines,
  type Op,
  parsePatch,
  rebaseOps,
  renderNumbered,
  type Section,
  tagOf,
  toLines,
} from "./hashedit.ts";

/**
 * What a session last saw at a path. Bounded per session: long sessions touch
 * many files and only the recent ones are plausible patch targets, so the oldest
 * entry is dropped rather than growing this without limit. A dropped snapshot
 * costs a re-read, never a wrong edit.
 */
const MAX_SNAPSHOTS_PER_SESSION = 64;
/**
 * …and a bound on sessions too. The server runs for weeks, so keying by session
 * without a cap is a slow leak; evicting the least recently active one costs at
 * worst a re-read in a session nobody has touched in a long time.
 */
const MAX_SESSIONS = 32;
const store = new Map<string, Map<string, string>>();

function snapshotsFor(sessionId: string): Map<string, string> {
  const existing = store.get(sessionId);
  // Re-insert on every touch so Map order is least-recently-used first.
  if (existing) {
    store.delete(sessionId);
    store.set(sessionId, existing);
    return existing;
  }
  const m = new Map<string, string>();
  store.set(sessionId, m);
  while (store.size > MAX_SESSIONS) store.delete(store.keys().next().value as string);
  return m;
}

/** Remember the text a session just saw (called by view and by a successful patch). */
export function recordSnapshot(sessionId: string | undefined, path: string, text: string): void {
  const m = snapshotsFor(sessionId ?? "-");
  m.delete(path); // re-insert so Map iteration order tracks recency
  m.set(path, text);
  while (m.size > MAX_SNAPSHOTS_PER_SESSION) m.delete(m.keys().next().value as string);
}

function snapshotOf(sessionId: string | undefined, path: string): string | undefined {
  return store.get(sessionId ?? "-")?.get(path);
}

const viewSchema = z.object({
  path: z.string().describe("File path, absolute or relative to the workspace."),
});

export const viewFile: ToolDef = {
  name: "view_file",
  description: "Read a file as numbered lines under a content tag, for editing with patch(). " +
    "Returns `[path#TAG]` followed by `N:text` lines.",
  schema: viewSchema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { path } = input as z.infer<typeof viewSchema>;
    const full = resolveInWorkspace(ctx, path);
    let text: string;
    try {
      text = await Deno.readTextFile(full);
    } catch (e) {
      throw new Error(`cannot read ${path}: ${(e as Error).message}`);
    }
    recordSnapshot(ctx.sessionId, path, text);
    return renderNumbered(path, text);
  },
};

/** Quote the lines an op names, so a conflict is fixable without another read. */
function quoteSpan(lines: string[], a: number, b: number, marker = "*"): string {
  const from = Math.max(1, a - 2);
  const to = Math.min(lines.length, b + 2);
  const width = String(to).length;
  const out: string[] = [];
  for (let i = from; i <= to; i++) {
    const flag = i >= a && i <= b ? marker : " ";
    out.push(`${flag}${String(i).padStart(width)}:${lines[i - 1]}`);
  }
  return out.join("\n");
}

/**
 * Resolve one section against the file on disk. Returns the new text, or throws
 * with a message that tells the model exactly what to do next.
 */
function resolveSection(
  sec: Section,
  current: string,
  base: string | undefined,
): { text: string; tag: string } {
  const curLines = toLines(current);
  const curTag = tagOf(current);
  let ops: Op[] = sec.ops;

  if (sec.tag !== curTag) {
    // The file moved under this patch. Without the text the agent read there is
    // nothing to rebase from; with it, try.
    if (base === undefined || tagOf(base) !== sec.tag) {
      throw new Error(
        `${sec.path} changed since it was read: the patch is anchored to #${sec.tag} ` +
          `but the file is now #${curTag}, and the version #${sec.tag} is no longer ` +
          `held. Re-read it with view("${sec.path}") and rewrite the patch against ` +
          `the fresh line numbers.`,
      );
    }
    const rebased = rebaseOps(sec.ops, toLines(base), curLines);
    if (!rebased.ok) {
      const detail = rebased.conflicts.map((c) =>
        `  ${c.op.kind.toUpperCase()} ${c.op.a}.=${c.op.b}: ${c.reason}`
      ).join("\n");
      const first = rebased.conflicts[0].op;
      throw new Error(
        `${sec.path} was edited by someone else where this patch writes ` +
          `(anchored #${sec.tag}, now #${curTag}):\n${detail}\n\n` +
          `The file now reads:\n${quoteSpan(curLines, first.a ?? 1, first.b ?? 1)}\n\n` +
          `Nothing was written. Re-read with view("${sec.path}") and reapply your ` +
          `change on top of theirs — do not overwrite it.`,
      );
    }
    ops = rebased.ops;
  }

  checkOps(ops, curLines.length);
  return { text: joinLines(applyOps(curLines, ops), current), tag: sec.tag };
}

const patchSchema = z.object({
  input: z.string().describe(
    "One or more file sections. Each starts with `[path#TAG]` where TAG is the " +
      "four-hex tag from view(path), followed by operations: `SWAP A.=B:` " +
      "(replace lines A..B), `DEL A.=B`, `INS.PRE A:`, `INS.POST A:`, `INS.HEAD:`, " +
      "`INS.TAIL:`. Body rows are `+`-prefixed new text; `+` alone is a blank line. " +
      "Line numbers are always in the coordinates of the tagged version.",
  ),
});

export const patchFile: ToolDef = {
  name: "patch_file",
  description: "Apply hash-anchored line edits to one or more files. Anchors are line numbers " +
    "bound to the content tag from view(), so a file edited concurrently is either " +
    "rebased automatically or reported as a conflict — never silently overwritten.",
  schema: patchSchema,
  async run(input: unknown, ctx: ToolRunCtx): Promise<string> {
    const { input: text } = input as z.infer<typeof patchSchema>;
    const sections = parsePatch(text);

    // Preflight every section before writing any of them: a patch that lands
    // half its files leaves the tree in a state nobody planned, and the agent
    // cannot tell which half from an error message.
    const staged: Array<{ path: string; full: string; text: string }> = [];
    for (const sec of sections) {
      const full = resolveInWorkspace(ctx, sec.path);
      let current: string;
      try {
        current = await Deno.readTextFile(full);
      } catch (e) {
        throw new Error(`cannot read ${sec.path}: ${(e as Error).message}`);
      }
      const { text: next } = resolveSection(sec, current, snapshotOf(ctx.sessionId, sec.path));
      staged.push({ path: sec.path, full, text: next });
    }

    const out: string[] = [];
    for (const s of staged) {
      try {
        await Deno.writeTextFile(s.full, s.text);
      } catch (e) {
        throw new Error(`cannot write ${s.path}: ${(e as Error).message}`);
      }
      // The session's view of the file is now what it just wrote, so a follow-up
      // patch in the same round can anchor to the tag echoed here without re-reading.
      recordSnapshot(ctx.sessionId, s.path, s.text);
      out.push(`[${s.path}#${tagOf(s.text)}] patched`);
    }
    return out.join("\n");
  },
};
