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
   * When set, the turn is sandboxed: bash runs inside the session's VM, and the
   * in-process file tools may also write under `sessionDir` (the clonefile snapshot
   * dir) and `scratchDir` (the per-session scratchpad — outside the repo, so temp
   * files don't pollute what the server builds or what ships). Absent for tests and
   * non-sandboxed runs.
   */
  sandbox?: { sessionDir: string; scratchDir: string };
  /**
   * Per-turn harness state, created by the turn runner. `check` is the committed
   * completion gate (SPEC §5): the shell command `run_steps` re-runs before
   * accepting `done`. Absent for tools that don't participate in gating.
   */
  turn?: {
    check?: string;
    checkNudged?: boolean;
    todo?: string;
    probeRounds?: number;
    everWrote?: boolean;
    /** Last agent-run bash command that exited 0 — cited verbatim by the probe
     * nag and the check-less done bounce as the ready-made `check` candidate,
     * so the model commits the verification it already ran instead of
     * re-verifying for more rounds (bench: the post-success probe tail). */
    lastGreenCmd?: string;
    /** Set (to the triggering request text) only for multi-rule requests —
     * enables the one-time spec-recheck bounce at done-time. */
    requestText?: string;
    specEchoed?: boolean;
    /** Subagents whose blocking agent() result carried changed files and that
     * were not adopt()ed yet this turn (id → title) — recorded by the turn
     * runner's delegate wiring, read by its adopt stop-gate. */
    unadopted?: Map<string, string>;
    /** True once this turn ran a parallel/background primitive (agent, spawn,
     * bashBg) — read by the turn runner's parallelism-honesty stop-gate. */
    ranParallel?: boolean;
  };
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
   * Ask the HUMAN a mid-task question (asks.ts, wired by the turn runner for every
   * supervisor turn). Blocks user-paced until the TUI answers; resolves with the
   * chosen option or typed free text. Rejects with a catchable "user declined"
   * error on decline, and rejects on turn interrupt so the program unwinds.
   */
  ask?: (question: string, opts?: { options?: string[] }) => Promise<string>;
  /**
   * Post a harness note to this session (→ turn.ts postSystemNote), waking a fresh
   * turn if idle or riding the queued-drain if one is running. Wired for every
   * supervisor turn; background shells (bash_bg.ts) use it to announce completion so
   * the model is told a job finished instead of polling for it.
   */
  notify?: (text: string) => void;
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
   * dir, hosts it on the bough server, and returns the artifact — its same-origin
   * `url` and absolute local `href`.
   */
  artifact?: (name: string, content: string) => Promise<Artifact>;
  /**
   * Semantic search over ALL past conversations (recall.ts, local embeddings),
   * wired by the turn runner for every supervisor turn. Runs host-side — the
   * sandbox never touches the DB or the embedder. Lazily indexes a batch of new
   * messages per call; the result's `indexed` field > 0 means the index is still
   * converging (call again for fuller coverage).
   */
  recall?: (query: string, k?: number) => Promise<unknown>;
  /**
   * Recurring runs (schedules.ts), wired by the turn runner for every supervisor
   * turn: the same validated CRUD the REST routes use, fanned out in the program
   * as the `schedule.*` method object (list/add/enable/disable/remove).
   */
  schedule?: {
    call: (verb: string, args: unknown) => Promise<unknown>;
  };
  /**
   * Workflows (workflow.ts), wired by the turn runner for root-session turns that
   * may delegate: scripted multi-agent orchestration, fanned out in the program as
   * the `workflow.*` method object (start/rerun/stop/pause/resume/status/list).
   * A started run is DETACHED from this turn — it survives the turn ending, and
   * its finished report arrives as a system note like a background subagent's.
   */
  workflow?: {
    call: (verb: string, args: unknown) => Promise<unknown>;
  };
  /**
   * Live program output, wired by the turn runner: fires for each console.*
   * line as run_steps' program prints it (→ a `tool.log` bus event the TUI
   * renders under the running tool call). Display-only — the model still
   * receives the joined logs in the tool result. The turn runner rebinds this
   * to the executing call before each tool runs, so it takes only the line.
   */
  onLog?: (line: string) => void;
  /**
   * Ship the session's work into the origin repo as a real commit (+ optional push)
   * — vcs/shadow.ts shipToOrigin via the turn runner. Wired only for root-session
   * turns whose workspace is a shadow worktree with a resolvable origin.
   */
  ship?: (opts: { message: string; paths?: string[]; push?: boolean }) => Promise<unknown>;
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
