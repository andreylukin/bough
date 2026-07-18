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
import type { Artifact } from "../server/artifacts.ts";

export interface ToolRunCtx {
  /** Absolute path the tool runs against (cwd for bash, root for file paths). */
  workspace: string;
  /** Session this turn belongs to — keys egress to its Claw Patrol listener + policy. */
  sessionId?: string;
  /**
   * The turn's interrupt signal. Long-running tools MUST observe it so the user's
   * stop button stops the actual work, not just the model stream: bash kills its
   * child process, run_steps terminates the in-flight program's worker. Absent in
   * contexts with no live turn (tests, one-off calls).
   */
  signal?: AbortSignal;
  /**
   * When set, the turn is sandboxed: bash wraps its argv in the Seatbelt profile
   * (darwin), and the in-process file tools may also write under `sessionDir` (the
   * clonefile snapshot dir) and `scratchDir` (the per-session scratchpad — outside
   * the repo, so temp files don't pollute what the server builds or what ships).
   * Absent for tests and non-sandboxed runs.
   */
  sandbox?: { sessionDir: string; scratchDir: string };
  /**
   * Per-turn harness state, created by the turn runner. `check` is the committed
   * completion gate (SPEC §5): the shell command `run_steps` re-runs before
   * accepting `done`. Absent for tools that don't participate in gating.
   */
  turn?: { check?: string };
  /**
   * Delegation, wired by the turn runner for sessions below the depth cap (absent
   * at MAX_SUBAGENT_DEPTH). Each subagent is a fresh session on its own workspace
   * branch. `run` blocks to completion; `spawn` starts one in the background and
   * returns its handle immediately (its finished report is delivered to the session
   * as a system note unless `join`ed first); `join` waits for a background
   * subagent's result in-band; `adopt` squashes a subagent's changes back into this
   * session's workspace. Subagent turns get blocking delegation only: `spawn`/`join`
   * are absent because a detached child would outlive the turn whose report was
   * already delivered upward.
   */
  delegate?: {
    run: (task: string) => Promise<unknown>;
    spawn?: (task: string) => Promise<unknown>;
    join?: (subagentSessionId: string) => Promise<unknown>;
    adopt: (subagentSessionId: string) => Promise<string>;
  };
  /**
   * The oracle (tools/oracle.ts), wired by the turn runner for every supervisor
   * turn: a read-only consult of a stronger reasoning model. The callback closes
   * over the turn's usage accumulators so oracle tokens bill to the session.
   */
  oracle?: (question: string) => Promise<string>;
  /**
   * MCP tool calls, wired by the turn runner when the triggering message's skills,
   * the session's manual activations, or a spawning turn's inherited grant
   * (subagents) granted servers. `call` runs the
   * session's Claw Patrol gate BEFORE the server sees the call — a deny rejects
   * with the policy reason, a hold blocks on human approval — and rejects for
   * servers outside the turn's grant. Absent = the program has no mcp().
   */
  mcp?: {
    call: (server: string, tool: string, args: unknown) => Promise<unknown>;
  };
  /**
   * MCP management state for the session (registry, auth, activations, live
   * connections — mcp/status.ts). Read-only and set for every supervisor turn:
   * status is not a capability grant, tool calls still require `mcp`.
   */
  mcpStatus?: () => Promise<unknown>;
  /**
   * LSP symbol verbs (mcp/lsp.ts), wired by the turn runner whenever the backing
   * language-intelligence server is registered — always-on, no skill grant, but
   * every call still passes the Claw Patrol gate like an `mcp` call. Lazy: the
   * first call connects the server and activates the session workspace.
   */
  lsp?: {
    call: (verb: string, args: unknown) => Promise<unknown>;
  };
  /**
   * Publish an artifact for browser viewing (server/artifacts.ts), wired by the turn
   * runner for every supervisor turn. Writes `content` under the session's artifact
   * dir, hosts it on the bough server, emits an `artifact.published` event, and
   * returns the artifact — its same-origin `url` and absolute local `href`.
   */
  artifact?: (name: string, content: string) => Promise<Artifact>;
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
 * must sit inside the workspace (or the sandbox's sessionDir / scratchDir, when
 * sandboxed). Seatbelt only guards subprocesses, so the in-process read/write/edit
 * tools enforce this themselves — including following symlinks, so a link inside the
 * workspace can't point the tool at a file outside it. Returns the lexical path to
 * operate on.
 */
export function resolveInWorkspace(ctx: ToolRunCtx, path: string): string {
  const full = resolve(ctx.workspace, path);
  const realFull = realPath(full);
  const roots = [ctx.workspace];
  if (ctx.sandbox) roots.push(ctx.sandbox.sessionDir, ctx.sandbox.scratchDir);
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
