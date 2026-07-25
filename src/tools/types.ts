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
import { resolve } from "node:path";
import type { Artifact } from "../server/artifacts.ts";

export interface ToolRunCtx {
  /** Absolute path the tool runs against (cwd for bash, root for file paths). */
  workspace: string;
  /** Session this turn belongs to — identity/attribution only (the tools all act
   * directly on the user's checkout; there is nothing per-session to key). */
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
    /** True once this turn ran a parallel/background primitive (agent, spawn,
     * bashBg) — read by the turn runner's parallelism-honesty stop-gate. */
    ranParallel?: boolean;
  };
  /**
   * Delegation, wired by the turn runner for sessions below the depth cap (absent
   * at MAX_SUBAGENT_DEPTH). Each subagent is a fresh session working in THIS
   * session's checkout. `run` blocks to completion; `spawn` starts one in the
   * background and returns its handle immediately (its finished report is delivered
   * to the session as a system note unless `join`ed first); `join` waits for a
   * background subagent's result in-band; `adopt` is a no-op explainer kept for
   * compatibility — there is nothing to merge. Subagent turns get blocking delegation only: `spawn`/`join`
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
   * (subagents) granted servers. `call` rejects for servers outside the turn's
   * grant. Absent = the program has no mcp().
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
   * language-intelligence server is registered — always-on, no skill grant. Lazy:
   * the first call connects the server and activates the session workspace.
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
   * Show an image file to the model (turn.ts), wired for every supervisor turn.
   * The program has no vision of its own — this copies the file into the
   * attachment store and posts it as a system note, so the picture reaches the
   * model with the same wake path as a background shell's completion note (i.e.
   * on the next turn, not inside the running program). Returns a one-line
   * confirmation; throws when the file is missing, unsupported, or too large.
   */
  image?: (path: string, note?: string) => Promise<string>;
  /**
   * Recurring runs (schedules.ts), wired by the turn runner for every supervisor
   * turn: the same validated CRUD the REST routes use, fanned out in the program
   * as the `schedule.*` method object (list/add/enable/disable/remove).
   */
  schedule?: {
    call: (verb: string, args: unknown) => Promise<unknown>;
  };
  /**
   * Durable key/value notes for this conversation (state.ts), wired by the turn
   * runner for every supervisor turn: fanned out in the program as the `state.*`
   * method object (get/set/list/delete). Scoped to the lineage's ROOT session, so
   * the notes outlive compaction, forks and the context cap.
   */
  state?: {
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
}

/**
 * Resolve `path` against the workspace. The workspace is the ORIGIN for relative
 * paths, not a boundary: an absolute path anywhere the user can reach resolves
 * unchanged, matching bash, which runs unconfined in the same places. Confinement
 * used to live here (and, for subprocesses, in a copy-on-write overlay); both were
 * removed because they cost more than they bought — see shellInvocation in
 * tools/bash.ts.
 */
export function resolveInWorkspace(ctx: ToolRunCtx, path: string): string {
  return resolve(ctx.workspace, path);
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
