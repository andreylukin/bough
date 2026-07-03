/**
 * The tool contract for the turn runner. A ToolDef pairs a Zod schema (used both
 * to derive the JSON Schema the Anthropic API sees and to validate Claude's input
 * before we execute) with a `run` that does the work in the session workspace.
 *
 * Convention: `run` returns the tool's textual output on success and *throws* on
 * failure. The executor (turn.ts) turns a thrown error into a tool_result with
 * `is_error: true`. A non-zero shell exit is normal output, not a thrown error —
 * bash reports the code in its text so Claude can react to it.
 */
import { z } from "zod/v4";
import { basename, dirname, join, resolve, sep } from "node:path";

export interface ToolRunCtx {
  /** Absolute path the tool runs against (cwd for bash, root for file paths). */
  workspace: string;
  /** Session this turn belongs to — keys egress to its Claw Patrol listener + policy. */
  sessionId?: string;
  /**
   * When set, the turn is sandboxed: bash wraps its argv in the Seatbelt profile
   * (darwin), and the in-process file tools may also write under `sessionDir` (the
   * clonefile snapshot dir). Absent for tests and non-sandboxed runs.
   */
  sandbox?: { sessionDir: string };
  /**
   * Per-turn harness state, created by the turn runner. `check` is the committed
   * completion gate (SPEC §5): the shell command `run_steps` re-runs before
   * accepting `done`. Absent for tools that don't participate in gating.
   */
  turn?: { check?: string };
}

/**
 * The real path of `p`, resolving symlinks. Since `p` may not exist yet (a file
 * being created), realpath the deepest existing ancestor and re-attach the missing
 * tail — so a symlinked workspace or a symlinked intermediate dir is followed, but a
 * not-yet-created leaf still resolves.
 */
function realPath(p: string): string {
  const tail: string[] = [];
  let cur = p;
  for (;;) {
    try {
      const real = Deno.realPathSync(cur);
      return tail.length ? join(real, ...tail.reverse()) : real;
    } catch {
      const parent = dirname(cur);
      if (parent === cur) return p; // reached the root; nothing resolved
      tail.push(basename(cur));
      cur = parent;
    }
  }
}

/**
 * Resolve `path` against the workspace and confine it: the symlink-resolved result
 * must sit inside the workspace (or the sandbox's sessionDir, when sandboxed).
 * Seatbelt only guards subprocesses, so the in-process read/write/edit tools enforce
 * this themselves — including following symlinks, so a link inside the workspace
 * can't point the tool at a file outside it. Returns the lexical path to operate on.
 */
export function resolveInWorkspace(ctx: ToolRunCtx, path: string): string {
  const full = resolve(ctx.workspace, path);
  const realFull = realPath(full);
  const roots = [ctx.workspace];
  if (ctx.sandbox) roots.push(ctx.sandbox.sessionDir);
  for (const root of roots) {
    const realRoot = realPath(resolve(root));
    if (realFull === realRoot || realFull.startsWith(realRoot + sep)) return full;
  }
  throw new Error(`path escapes the workspace: ${path}`);
}

export interface ToolDef {
  name: string;
  description: string;
  schema: z.ZodType;
  run(input: unknown, ctx: ToolRunCtx): Promise<string>;
}

/** Draft-7 JSON Schema for the tool's input, as required by the Messages API. */
export function jsonSchema(tool: ToolDef): Record<string, unknown> {
  return z.toJSONSchema(tool.schema, { target: "draft-7" }) as Record<string, unknown>;
}
