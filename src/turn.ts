/**
 * The turn runner. A "turn" is what happens after a user message lands: we stream
 * an Anthropic response into a pending supervisor message, run any tools it asks
 * for, and loop until it stops — checkpointing a persisted state machine so a crash
 * can be recovered (see supervisor/turns.ts).
 *
 * Event contract (against the pending supervisor message's id):
 *   message.started  — emitted here when the placeholder row is created
 *   message.delta    — { messageId, delta } as text streams
 *   message.part     — { messageId, part } when a content block / tool result lands
 *   message.finished — { messageId } once the turn ends (success or error)
 *
 * The SDK is injected via ctx.llm (defaults to the real Anthropic client) so tests
 * drive the whole loop with a scripted fake and never hit the network.
 *
 * Replay mapping (stored parts → Anthropic messages), see toLlmMessages:
 *   - user message      → one user message of text blocks
 *   - supervisor/worker → an assistant message (text + tool_use) followed, if it
 *     produced tool results, by a user message of tool_result blocks
 *   - reasoning parts   → DROPPED on replay. They're persisted for display, but we
 *     don't run extended thinking, so there are no signed thinking blocks to echo;
 *     re-sending them as plain text would only confuse the model.
 *   - any tool_use without a matching tool_result (e.g. a crash mid-tool) gets a
 *     synthetic error tool_result so the history stays valid for the API.
 */
import { join } from "node:path";
import type { Db } from "./db/db.ts";
import type { Bus } from "./bus.ts";
import type { Message, Part } from "./schema/parts.ts";
import {
  defaultTools,
  DONE_ACCEPTED,
  jsonSchema,
  type ToolDef,
  type ToolRunCtx,
} from "./tools/mod.ts";
import {
  clientFor,
  type LlmClient,
  type LlmContentBlock,
  type LlmMessage,
} from "./supervisor/llm.ts";
import { checkpoint, finishTurn, startTurn } from "./supervisor/turns.ts";
import {
  adoptSubagent,
  joinSubagent,
  MAX_SPAWNS_PER_TURN,
  MAX_SUBAGENT_DEPTH,
  MAX_TREE_CONCURRENT,
  runSubagent,
  spawnSubagentDetached,
  subagentDepth,
} from "./subagent.ts";
import { maybeAutoTitle, type Titler } from "./supervisor/title.ts";
import { activeSkills } from "./supervisor/skills.ts";
import { prepareWorkspace } from "./supervisor/workspace.ts";
import { activationsFor } from "./mcp/config.ts";
import { createLspBridge, lspAvailable, lspSection } from "./mcp/lsp.ts";
import { mcpManager } from "./mcp/manager.ts";
import { mcpSection } from "./mcp/prompt.ts";
import { mcpStatusFor } from "./mcp/status.ts";
import { expandFileReferences } from "./server/files.ts";

export interface TurnCtx {
  db: Db;
  bus: Bus;
  /** Injected for tests; defaults to the real Anthropic client. */
  llm?: LlmClient;
  /** Title worker, injected for tests; defaults to local worker → frontier backstop. */
  titler?: Titler;
  /** Injected for tests; defaults to the built-in bash/read/write/edit set. */
  tools?: ToolDef[];
  /** Tool cwd; defaults to BOUGH_WORKSPACE or the process cwd. */
  workspace?: string;
  model?: string;
  /**
   * MCP servers inherited from a spawning turn. A subagent turn connects these in
   * addition to its own skills/activations — the human's grant to the spawner
   * extends to the subagents doing parts of that same granted work. Captured at
   * spawn time, so a later manual continuation of the subagent doesn't inherit.
   */
  mcpGrant?: string[];
}

/** Thrown to unwind the turn loop when the user interrupts. */
class InterruptedError extends Error {
  constructor() {
    super("interrupted");
    this.name = "InterruptedError";
  }
}

/**
 * The model turns run on. Starts from BOUGH_MODEL (else the default) and can be
 * changed at runtime via PATCH /config — new turns pick up the change. Anthropic
 * ids ("claude-…") route to the Anthropic client; an "openai:…" id routes to OpenAI
 * proper; any other "vendor/model" id routes to OpenRouter (see llm.clientFor).
 */
let currentModel = Deno.env.get("BOUGH_MODEL") ?? "claude-opus-4-8";

export function activeModel(): string {
  return currentModel;
}

export function setActiveModel(model: string): void {
  currentModel = model;
}

/**
 * Models offered in the picker. Anthropic ids go direct; "openai:…" ids go to OpenAI
 * (need OPENAI_API_KEY); "vendor/model" ids go through OpenRouter (need
 * OPENROUTER_API_KEY). Not exhaustive — the composer accepts any id, this is just the
 * quick-switch menu.
 */
export interface ModelRow {
  id: string;
  label: string;
  provider: "anthropic" | "openai" | "openrouter";
  /** USD per million tokens (input/output) — drives the UI's cost estimate. */
  pricing?: { in: number; out: number };
}
export const MODELS: ModelRow[] = [
  { id: "claude-opus-4-8", label: "Opus 4.8", provider: "anthropic", pricing: { in: 5, out: 25 } },
  { id: "claude-fable-5", label: "Fable 5", provider: "anthropic", pricing: { in: 10, out: 50 } },
  { id: "claude-sonnet-5", label: "Sonnet 5", provider: "anthropic", pricing: { in: 3, out: 15 } },
  { id: "claude-haiku-4-5", label: "Haiku 4.5", provider: "anthropic", pricing: { in: 1, out: 5 } },
  { id: "openai:gpt-5", label: "GPT-5 (OpenAI)", provider: "openai" },
  { id: "openai:gpt-5-mini", label: "GPT-5 mini (OpenAI)", provider: "openai" },
  { id: "openai/gpt-5", label: "GPT-5 (OpenRouter)", provider: "openrouter" },
  { id: "google/gemini-2.5-pro", label: "Gemini 2.5 Pro (OpenRouter)", provider: "openrouter" },
  { id: "z-ai/glm-5.2", label: "GLM 5.2 (OpenRouter)", provider: "openrouter" },
];

const MAX_TOKENS = 64_000;
// Code-mode (SPEC §5): the supervisor plans and writes; the harness is the only
// executor. One program per round, CHECK-gated completion.
// Explicit turn ending: the model must CALL `stop` to end its turn — a response
// that just trails off (text with no tool call) gets nudged to continue or stop.
// The stop call and the nudges live only in this turn's in-memory exchange; they
// are never persisted, so the thread and every future prompt replay stay clean.
const STOP_NAME = "stop";
const STOP_TOOL = {
  name: STOP_NAME,
  description: "End your turn. Call this after your final text, in the same response, once the " +
    "user's request is fully handled. Your turn does not end until you call it.",
  inputSchema: { type: "object", properties: {}, additionalProperties: false } as Record<
    string,
    unknown
  >,
};
/** Re-prompts before the harness gives up on an explicit stop (runaway brake). */
const MAX_STOP_NUDGES = 3;
const STOP_NUDGE = "[harness] Your turn is still open — it only ends when you call the stop " +
  "tool. Continue if there is more to do, or call stop now (alone, no other output) if you " +
  "are finished.";

const SYSTEM = [
  "You are bough, a coding agent. You act ONLY through the run_steps tool: each call",
  "carries one JavaScript program that a deterministic harness executes in a sealed V8",
  "sandbox — you never touch the machine directly.",
  "Inside the program the core capability surface is four async host functions:",
  "await bash(cmd) — shell in the sandboxed workspace, returns combined output;",
  "await read(path); await write(path, content); await edit(path, oldText, newText).",
  "Later sections of this prompt may grant more host functions — delegation",
  "(agent/spawn/join/adopt), await mcp(server, tool, args) for MCP tools (whose",
  "connected servers and calling convention appear in a '# MCP tools' section), and",
  "lsp.* symbol navigation (a '## Symbol navigation (lsp)' section). A host",
  "function exists ONLY when this prompt grants it — never guess at others.",
  "One host function is always available: await mcpStatus() returns this session's",
  "MCP management state {registry, auth, active, connections}. MCP servers are",
  "managed through bough itself, NOT through other tools' config files. Answer any",
  "MCP question from a FRESH mcpStatus() call, never from conversation memory —",
  "registry entries, grants, and connections change between turns (UI toggles, other",
  "sessions, TTL lapses). For changes (register/enable/auth) tell the human to type",
  "/mcp instead of improvising.",
  "console.log(...) is how you see anything — print ONLY what the next round needs.",
  "Program output is billed context: filter at the source (rg/head/tail/wc, targeted",
  "reads) instead of dumping whole files or raw command output, and never re-print",
  "content you already have in context.",
  "Search code with rg (ripgrep — installed) instead of grep -r or find sweeps; and",
  "when this prompt has a '## Symbol navigation (lsp)' section, START code exploration",
  "with those verbs — a symbol overview or reference list is far cheaper and more",
  "precise than dumped files.",
  "Granted tooling can still break at runtime (an lsp language server missing, an MCP",
  "server down). That is NEVER a reason to stop or declare the task blocked: mention",
  "the failure in one line and finish the job with bash/rg/read.",
  "The sandbox HAS network access: outbound requests from bash (curl, git, package",
  "managers) pass through a human-supervised egress gate. ATTEMPT network commands",
  "instead of declaring the network unavailable — an unapproved host parks the request",
  "for the human to approve (the command may block briefly), and a denial returns an",
  "explicit egress-denied error, which you report without retrying.",
  "Write one program per round covering inspect → change → verify; prefer one",
  "substantial program over many tiny rounds.",
  "Commit a `check` early: a shell command that exits 0 iff the task's literal",
  "acceptance criteria hold. Set `done: true` when the work is complete — the harness",
  "re-runs the committed check and accepts done only if it passes.",
  "Your turn NEVER ends on its own: when the user's request is fully handled, call the",
  "stop tool — after your final text, in the same response. Ending without stop just",
  "gets you re-prompted to continue.",
  "For pure questions or conversation, answer in plain text without calling run_steps,",
  "then call stop in the same response.",
  "Text output renders in a compact chat UI. Be terse: answer in 1-3 short lines unless",
  "the user asks for detail; one-word answers are fine. After work, report outcome only —",
  "what changed and whether the check passed — never a step-by-step narration.",
  "Cut filler from every output, chat text and program prints alike: no preambles",
  '("Let me...", "I\'ll now..."), no postambles, no hedging without information',
  '("seems to", "might possibly"), no restating the question, no meta-commentary or',
  'apologies. "X imports Y" beats "It looks like X seems to import Y" — specificity',
  "comes from content, not phrasing. Act, then stop.",
].join(" ");

// Delegation section, appended only for sessions that may spawn (not subagents).
const SYSTEM_DELEGATION = " " + [
  "More host functions enable delegation to subagents — separate sessions, each working",
  "on its own branched copy of the workspace. await spawn(task) starts one in the",
  "BACKGROUND and returns {sessionId, title} immediately: keep working, or end your turn —",
  "when it finishes, its report arrives as a [subagent finished] system message and wakes",
  "you if you're idle. await join(sessionId) instead waits for a background subagent and",
  "returns its full result in-band. await agent(task) is the blocking shorthand",
  "(spawn+join): it runs the task to completion and returns {sessionId, ok, checkPassed,",
  "report, changedFiles}. Subagents start with NO context beyond the task string: include",
  "every relevant path, constraint, and acceptance criterion in it. They DO inherit this",
  "turn's MCP servers — a subagent's program can call the same mcp() tools (each call",
  "still passes the egress gate), so delegating MCP-dependent work is fine; name the",
  "server and tool in the task. Their file changes",
  "stay on their own branch — call await adopt(sessionId) to merge a subagent's changes",
  "into your workspace, or leave the branch for the user to review. Prefer spawn for",
  "long tasks so you stay responsive; run independent blocking subtasks concurrently with",
  "Promise.all. Subagents can delegate one level further themselves (blocking only).",
  `Caps: at most ${MAX_SPAWNS_PER_TURN} spawns per turn and ${MAX_TREE_CONCURRENT} subagents`,
  "running at once across the whole tree — a spawn beyond a cap fails with an error,",
  "so plan batches accordingly.",
  "Delegate only genuinely separable work; do small things yourself.",
].join(" ");

// Appended for every subagent turn: its final text is the report consumed by the
// spawner, so cap it — verbose reports bloat the parent's context.
const SYSTEM_SUBAGENT = " " + [
  "You are a subagent: your final text is the report returned to your spawner, not a",
  "user-facing message. Keep it to what the spawner needs — outcome, files changed,",
  "check status, and any surprises — in a few short lines.",
].join(" ");

// Reduced delegation section for subagent turns: blocking only. A detached spawn
// could outlive this turn and mutate the branch after its report went upward.
const SYSTEM_DELEGATION_NESTED = " " + [
  "More host functions enable delegation: await agent(task) runs a nested subagent to",
  "completion on its own branched copy of this workspace and returns {sessionId, ok,",
  "checkPassed, report, changedFiles}. Nested subagents start with NO context beyond the",
  "task string — include every relevant path, constraint, and acceptance criterion in",
  "it — and cannot delegate further. They inherit this turn's MCP servers (their",
  "programs can call the same mcp() tools). Their file changes stay on their own branch: call",
  "await adopt(sessionId) to merge them into your workspace so they are part of your",
  "result. Run independent blocking subtasks concurrently with Promise.all. Caps: at",
  `most ${MAX_SPAWNS_PER_TURN} spawns per turn and ${MAX_TREE_CONCURRENT} subagents running`,
  "at once across the whole tree — a spawn beyond a cap fails with an error. Delegate",
  "only genuinely separable work; do small things yourself.",
].join(" ");

/**
 * A system-prompt section listing this session's background subagents that are
 * still running, so the model stays aware of in-flight delegated work across
 * turns — it can join() one or simply not re-delegate the same task. Empty when
 * nothing is running.
 */
function runningSubagentsNote(db: Db, sessionId: string): string {
  const running = db.listSessions().filter((s) =>
    s.kind === "subagent" && s.originId === sessionId && isTurnRunning(s.id)
  );
  if (running.length === 0) return "";
  return "\n\n# Background subagents currently running\n" +
    running.map((s) =>
      `- "${s.title}" (${s.id}) — join("${s.id}") to wait for its result, or end your turn and its report will arrive as a system note.`
    ).join("\n");
}

/**
 * Tell the model where its tools actually operate. Without this it has zero cwd
 * information and tends to invent a container layout (`cd /workspace || cd /home`),
 * walking itself out of the real project.
 */
function workspaceNote(cwd: string): string {
  return `\n\n# Workspace\nbash starts in ${cwd} and relative file paths resolve ` +
    "against it. If that directory is a repo, it is the repo the user means — work on " +
    "it in place.";
}

/**
 * Point the agent at the per-session scratchpad for throwaway files. The workspace is
 * a repo the live server builds from and jj auto-snapshots, so a stray `./probe.json`
 * pollutes the build and `git diff main HEAD`. The scratch dir is outside the repo and
 * OS-reaped — the right home for anything not meant to ship.
 */
function scratchpadNote(scratchDir: string): string {
  return `\n\n# Scratchpad\n${scratchDir} is a writable per-session temp dir OUTSIDE ` +
    "the workspace. Put ALL throwaway files there — intermediate data, temp scripts, " +
    "probe outputs, downloads — NOT in the workspace and NOT in /tmp. Files written " +
    "into the workspace are treated as real changes: they get snapshotted, built by " +
    "the live server, and show up in the diff you're asked to ship. Use absolute paths " +
    "under the scratchpad; it's already created.";
}

/** Read the workspace AGENTS.md (capped) as a system-prompt section, or null if absent. */
async function readAgentsFile(cwd: string): Promise<string | null> {
  try {
    const text = await Deno.readTextFile(join(cwd, "AGENTS.md"));
    if (!text.trim()) return null;
    // Cap so a huge file can't crowd out the task; the model can read the rest itself.
    const body = text.length > 12_000 ? text.slice(0, 12_000) + "\n…(truncated)" : text;
    return "\n\n# Project rules (AGENTS.md)\nThe workspace root has an AGENTS.md — treat it " +
      'as authoritative for build/test commands, conventions, and what "done" means:\n\n' +
      body.trim();
  } catch {
    return null;
  }
}

/** Kick off the supervisor turn for `sessionId`. Returns the placeholder message. */
export function runTurn(ctx: TurnCtx, sessionId: string): Message {
  return beginTurn(ctx, sessionId).message;
}

/**
 * Like runTurn, but also hands back the promise that resolves when the turn is
 * fully done. runTurn discards it (fire-and-forget for the 202 path); tests await it.
 */
export function beginTurn(
  ctx: TurnCtx,
  sessionId: string,
): { message: Message; done: Promise<void> } {
  const message: Message = {
    id: crypto.randomUUID(),
    sessionId,
    role: "supervisor",
    parts: [],
    pending: true,
    createdAt: Date.now(),
  };
  ctx.db.createMessage(message);
  ctx.bus.publish({ type: "message.started", sessionId, data: message });
  // Register an abort handle so `interruptTurn(sessionId)` can stop this turn
  // between rounds / tools. Keyed by session — one live turn per session.
  const controller = new AbortController();
  running.set(sessionId, controller);
  const done = drive(ctx, message, controller.signal).catch((err) => {
    // drive() handles its own failures; this guards against a truly unexpected throw.
    console.error("turn runner crashed:", err);
  }).finally(() => {
    if (running.get(sessionId) === controller) running.delete(sessionId);
    // Drain a message queued while this turn ran: run one follow-up turn, which
    // sees every queued user message since the last supervisor reply. A steered
    // message rides the same drain — its flag just ended the loop early.
    steering.delete(sessionId);
    if (queued.delete(sessionId)) beginTurn(ctx, sessionId);
  });
  return { message, done };
}

/** Sessions with a user message queued while a turn was in flight (see startUserTurn). */
const queued = new Set<string>();

/**
 * Sessions whose in-flight turn should yield at the next round boundary because a
 * user message steered in mid-turn (see startUserTurn). The current LLM round and
 * its tools finish normally — nothing is killed — then the loop stops instead of
 * asking the model for another round, and the queued-drain follow-up turn runs
 * immediately with the new message in history. Steer = the queue, minus the wait.
 */
const steering = new Set<string>();

/** Live turns by session, for interruption. One turn per session at a time. */
const running = new Map<string, AbortController>();

/** True if a turn is currently running for this session. */
export function isTurnRunning(sessionId: string): boolean {
  return running.has(sessionId);
}

/**
 * Interrupt the session's in-flight turn. Aborts the current LLM request and
 * signals the loop to stop after the current step. Returns false if nothing runs.
 */
export function interruptTurn(sessionId: string): boolean {
  const c = running.get(sessionId);
  if (!c) return false;
  c.abort();
  return true;
}

/**
 * Post a user message and run a turn from it — the shared "user speaks" path behind
 * both POST /sessions/:id/messages and fork-with-edit. Persists the user message,
 * announces it (message.started), and kicks off the supervisor turn. `done` resolves
 * when the turn finishes (tests await it; the HTTP path discards it).
 */
export function startUserTurn(
  ctx: TurnCtx,
  sessionId: string,
  text: string,
): { userMessage: Message; done: Promise<void> } {
  const userMessage: Message = {
    id: crypto.randomUUID(),
    sessionId,
    role: "user",
    parts: [{ type: "text", text }],
    pending: false,
    createdAt: Date.now(),
  };
  ctx.db.createMessage(userMessage);
  ctx.bus.publish({ type: "message.started", sessionId, data: userMessage });
  // Fire-and-forget: name an untitled session from its first message (title worker).
  maybeAutoTitle({ db: ctx.db, bus: ctx.bus, titler: ctx.titler }, sessionId, text);
  // One turn per session: if one is already running, the message is persisted and
  // shown now, and it STEERS — the live turn yields at its next round boundary and
  // the follow-up turn (which sees this message) starts immediately. Clients that
  // want plain queueing hold the message until the turn finishes (the web UI does).
  if (isTurnRunning(sessionId)) {
    queued.add(sessionId);
    steering.add(sessionId);
    return { userMessage, done: Promise.resolve() };
  }
  const { done } = beginTurn(ctx, sessionId);
  return { userMessage, done };
}

/**
 * Deliver a harness note (role "system") to a session and make sure a turn sees it:
 * starts one if the session is idle, else the queued-drain follow-up picks it up.
 * This is how a background subagent's finished report wakes its spawner.
 */
export function postSystemNote(ctx: TurnCtx, sessionId: string, text: string): void {
  const msg: Message = {
    id: crypto.randomUUID(),
    sessionId,
    role: "system",
    parts: [{ type: "text", text }],
    pending: false,
    createdAt: Date.now(),
  };
  ctx.db.createMessage(msg);
  ctx.bus.publish({ type: "message.started", sessionId, data: msg });
  if (isTurnRunning(sessionId)) queued.add(sessionId);
  else beginTurn(ctx, sessionId);
}

async function drive(ctx: TurnCtx, message: Message, signal?: AbortSignal): Promise<void> {
  const { db, bus } = ctx;
  const sessionId = message.sessionId;
  const messageId = message.id;
  // Model precedence: explicit ctx override (tests/embedders) → the session's own
  // pinned model (set when the user switches models with this session open) → the
  // process-global default (what new sessions start on).
  const model = ctx.model ?? db.getSession(sessionId)?.model ?? activeModel();
  const llm = ctx.llm ?? clientFor(model);
  const tools = ctx.tools ?? defaultTools;
  // Newest round's input_tokens ≈ the live context size; output accumulates, and
  // input accumulates too (cost: every round re-sends the whole thread).
  let contextTokens = 0;
  let outputTokens = 0;
  let inputTokens = 0;
  // Last round's cached prompt share + finish time — the cache-warmth clock the
  // tree view decays against (Anthropic's 5-min sliding TTL starts here).
  let cachedTokens = 0;
  let lastLlmAt = 0;

  const turn = startTurn(db, sessionId, messageId);
  const parts: Part[] = [];
  const append = (part: Part) => {
    parts.push(part);
    db.updateMessage(messageId, parts, true);
    bus.publish({ type: "message.part", sessionId, data: { messageId, part } });
  };

  try {
    // Resolve the workspace and (if sandboxed) set up jj + the snapshot dir once.
    const prepared = await prepareWorkspace(db, sessionId, ctx.workspace);
    if (prepared.warning) {
      // Isolation degraded — surface it in the thread (plain insert, not
      // postSystemNote: we're already inside a turn, nothing to wake).
      const note: Message = {
        id: crypto.randomUUID(),
        sessionId,
        role: "system",
        parts: [{ type: "text", text: prepared.warning }],
        pending: false,
        createdAt: Date.now(),
      };
      db.createMessage(note);
      bus.publish({ type: "message.started", sessionId, data: note });
    }
    const toolCtx: ToolRunCtx = {
      workspace: prepared.cwd,
      sessionId,
      // Interrupt reaches INTO a running tool: run_steps terminates its program's
      // worker and bash kills its child — stop means stop, not "after this step".
      signal,
      sandbox: prepared.sandboxed
        ? { sessionDir: prepared.sessionDir, scratchDir: prepared.scratchDir }
        : undefined,
      // Per-turn harness state: run_steps commits the CHECK here (SPEC §5 gating).
      turn: {},
      // Management-plane visibility, always on: the program can ask what MCP state
      // this session sees without shelling curl at the loopback API. Read-only —
      // tool calls still enter through the granted mcp() bridge below.
      mcpStatus: () => Promise.resolve(mcpStatusFor(sessionId)),
    };
    // Delegation, allowed below the depth cap. Subagent turns get BLOCKING
    // delegation only (agent/adopt): a detached spawn would outlive the turn whose
    // report already went upward. At the cap there's no delegate at all, so those
    // programs have no delegation host functions and the prompt never mentions them.
    const session = db.getSession(sessionId);
    const isSub = session?.kind === "subagent";
    // Skills: `/name` in the triggering user message pulls that skill's
    // instructions into the system prompt for this run (supervisor/skills.ts) and
    // names the MCP servers the invocation grants.
    const skills = activeSkills(lastUserText(db, sessionId));
    // The turn's MCP grant: the invoked skills' servers + the session's manual
    // activations (/mcp enable) + servers inherited from the spawning turn (a
    // subagent doing part of granted work keeps the grant).
    const grantedMcp = [
      ...new Set([...skills.servers, ...activationsFor(sessionId), ...(ctx.mcpGrant ?? [])]),
    ].sort();
    const mayDelegate = session !== undefined &&
      subagentDepth(db, sessionId) < MAX_SUBAGENT_DEPTH;
    if (mayDelegate) {
      const sctx = { spawnerId: sessionId, spawnerMessageId: messageId, model, signal };
      // Subagents inherit this turn's MCP grant (captured now, not at call time).
      const subCtx: TurnCtx = { ...ctx, mcpGrant: grantedMcp };
      toolCtx.delegate = {
        run: (task) => runSubagent(subCtx, sctx, task),
        adopt: (subId) => adoptSubagent(ctx, sessionId, subId),
        ...(isSub ? {} : {
          spawn: (task) => spawnSubagentDetached(subCtx, sctx, task),
          join: (subId) => joinSubagent(sctx, subId),
        }),
      };
    }

    const messages = buildHistory(db, sessionId, messageId);
    // Anything queued/steered before this point is already in the history we just
    // built — clear the flags so it isn't re-delivered by a spurious follow-up
    // turn or an instant yield below.
    queued.delete(sessionId);
    steering.delete(sessionId);
    // Inline any @path references in the triggering message so the model sees the
    // file content, not just the name. Only for workspace sessions (files exist).
    if (prepared.sandboxed) inlineFileReferences(messages, prepared.cwd);
    // Project rules: an AGENTS.md at the workspace root is authoritative for build/test
    // commands, conventions, and what "done" means — inject it into the system prompt.
    const agents = prepared.sandboxed ? await readAgentsFile(prepared.cwd) : null;
    // MCP: connect the turn's granted servers (grantedMcp, resolved above) so the
    // prompt can list real tools; a server that fails to connect is named
    // UNAVAILABLE instead of vanishing. Subagent turns connect their inherited
    // grant here too — under their own session id, so gating and workspace stay
    // scoped to the subagent.
    let mcpNote = "";
    if (grantedMcp.length > 0) {
      const catalog = await mcpManager().ensure(sessionId, grantedMcp, {
        workspace: prepared.cwd,
        sandbox: toolCtx.sandbox,
      });
      mcpNote = mcpSection(catalog);
      const usable = new Set(catalog.filter((c) => !c.error).map((c) => c.name));
      if (usable.size > 0) {
        toolCtx.mcp = {
          call: (server, tool, args) =>
            usable.has(server)
              ? mcpManager().call(sessionId, server, tool, args)
              : Promise.reject(new Error(`mcp server "${server}" is not granted for this turn`)),
        };
      }
    }
    // LSP: always-on when the backing server is registered — symbol navigation is
    // a core capability, not a skill grant. Nothing spawns until the program's
    // first lsp.* call, and every underlying tool call still passes the Claw
    // Patrol gate exactly like mcp().
    let lspNote = "";
    if (lspAvailable()) {
      toolCtx.lsp = createLspBridge(
        sessionId,
        { workspace: prepared.cwd, sandbox: toolCtx.sandbox },
        mcpManager(),
      );
      lspNote = lspSection();
    }
    const system = SYSTEM +
      (isSub ? SYSTEM_SUBAGENT : "") +
      (mayDelegate ? (isSub ? SYSTEM_DELEGATION_NESTED : SYSTEM_DELEGATION) : "") +
      (mayDelegate && !isSub ? runningSubagentsNote(db, sessionId) : "") +
      workspaceNote(prepared.cwd) +
      (prepared.sandboxed ? scratchpadNote(prepared.scratchDir) : "") +
      (agents ?? "") +
      mcpNote + lspNote + skills.sections;
    const toolDefs = [
      ...tools.map((t) => ({
        name: t.name,
        description: t.description,
        inputSchema: jsonSchema(t),
      })),
      STOP_TOOL,
    ];

    // Unbounded on purpose: the loop ends when the model calls `stop` or the CHECK
    // gate accepts done; interruptTurn is the user's brake on a runaway.
    let nudges = 0;
    for (let round = 0;; round++) {
      if (signal?.aborted) throw new InterruptedError();
      // A user message steered in mid-turn: yield here (a clean round boundary —
      // every tool_use already has its tool_result) and let the follow-up turn
      // continue with the new message in history.
      if (steering.has(sessionId)) break;
      const result = await llm.run(
        { model, system, maxTokens: MAX_TOKENS, messages, tools: toolDefs },
        (delta) => bus.publish({ type: "message.delta", sessionId, data: { messageId, delta } }),
        signal,
      );

      if (result.usage) {
        contextTokens = result.usage.inputTokens; // last round = current context size
        outputTokens += result.usage.outputTokens;
        inputTokens += result.usage.inputTokens;
        cachedTokens = (result.usage.cacheReadTokens ?? 0) +
          (result.usage.cacheCreationTokens ?? 0);
        lastLlmAt = Date.now();
      }

      // The stop call is loop control, not content: honor it, but never persist
      // or replay it — the thread and future prompts must not carry it.
      let stopRequested = false;
      const assistant: LlmContentBlock[] = [];
      for (const block of result.content) {
        if (block.type === "text") {
          append({ type: "text", text: block.text });
          assistant.push({ type: "text", text: block.text });
        } else if (block.type === "reasoning") {
          // Persisted for display (when there's a summary to show) and replayed
          // in-memory within this turn — the OpenAI Responses client must echo a
          // function_call's reasoning item back on the next round. Cross-turn
          // replay still drops reasoning (see toLlmMessages).
          if (block.text) append({ type: "reasoning", text: block.text });
          assistant.push(block);
        } else if (block.type === "tool_use") {
          if (block.name === STOP_NAME) {
            stopRequested = true;
            continue;
          }
          append({ type: "tool_call", id: block.id, name: block.name, input: block.input });
          assistant.push(block);
        }
      }
      if (assistant.length > 0) messages.push({ role: "assistant", content: assistant });
      checkpoint(db, turn.id, `round:${round + 1}`);

      const toolUses = result.content.filter(
        (b) => b.type === "tool_use" && b.name !== STOP_NAME,
      );
      if (toolUses.length > 0) {
        const toolResults: LlmContentBlock[] = [];
        let doneAccepted = false;
        for (const tu of toolUses) {
          if (tu.type !== "tool_use") continue;
          // Don't start new tools once interrupted — stop before side effects.
          if (signal?.aborted) throw new InterruptedError();
          const { output, isError } = await executeTool(tools, tu.name, tu.input, toolCtx);
          append({ type: "tool_result", callId: tu.id, output, isError });
          toolResults.push({ type: "tool_result", toolUseId: tu.id, content: output, isError });
          checkpoint(db, turn.id, `tool:${tu.name}`);
          // CHECK-gated completion (SPEC §5): the harness — not the model's say-so —
          // decides `done`. run_steps stamps its verdict into the tool output.
          if (
            tu.name === "run_steps" &&
            (tu.input as { done?: boolean })?.done === true &&
            output.includes(DONE_ACCEPTED)
          ) {
            doneAccepted = true;
          }
        }
        messages.push({ role: "user", content: toolResults });
        if (doneAccepted || stopRequested) break;
        continue;
      }

      // No real tool calls this round: only an explicit stop ends the turn.
      if (stopRequested) break;
      // Trailed off without stop — re-prompt (in-memory only, never persisted),
      // with a cap so a stop-incapable model can't loop the API forever.
      if (nudges >= MAX_STOP_NUDGES) break;
      nudges++;
      messages.push({ role: "user", content: [{ type: "text", text: STOP_NUDGE }] });
    }

    db.updateMessage(messageId, parts, false);
    finishTurn(db, turn.id, "done");
    bus.publish({ type: "message.finished", sessionId, data: { messageId } });
    bus.publish({ type: "turn.finished", sessionId, data: { sessionId, status: "done" } });
  } catch (err) {
    // An interrupt (explicit abort, or the SDK's abort error) ends the turn
    // cleanly with a marker, not a failure — the user asked it to stop.
    const interrupted = err instanceof InterruptedError || signal?.aborted ||
      (err as Error)?.name === "APIUserAbortError" || (err as Error)?.name === "AbortError";
    const note: Part = interrupted
      ? { type: "text", text: "⏹ Stopped." }
      : { type: "text", text: `⚠︎ Turn failed: ${(err as Error).message}` };
    parts.push(note);
    db.updateMessage(messageId, parts, false);
    const status = interrupted ? "interrupted" : "error";
    finishTurn(db, turn.id, status);
    bus.publish({ type: "message.part", sessionId, data: { messageId, part: note } });
    bus.publish({ type: "message.finished", sessionId, data: { messageId } });
    bus.publish({ type: "turn.finished", sessionId, data: { sessionId, status } });
  } finally {
    // Persist token usage for the context meter and announce it (usage.updated).
    // contextTokens stays 0 for a stream that errored before its first usage report.
    if (contextTokens > 0 || outputTokens > 0) {
      const prev = db.sessionUsage(sessionId);
      const totals = {
        outputTokens: prev.outputTokens + outputTokens,
        inputTokens: prev.inputTokens + inputTokens,
      };
      db.setSessionUsage(sessionId, contextTokens, totals.outputTokens, totals.inputTokens);
      if (lastLlmAt > 0) db.setSessionCache(sessionId, cachedTokens, lastLlmAt);
      bus.publish({
        type: "usage.updated",
        sessionId,
        data: {
          sessionId,
          contextTokens,
          ...totals,
          tree: db.treeUsage(sessionId),
          ...(lastLlmAt > 0 ? { cachedTokens, lastLlmAt } : {}),
        },
      });
      // Cost rolls up the origin chain: nudge each ancestor with its refreshed tree
      // total, so the root's spend moves when a subagent (at any depth) burns tokens.
      for (
        let cur = db.getSession(sessionId);
        cur?.kind === "subagent" && cur.originId;
        cur = db.getSession(cur.originId)
      ) {
        const u = db.sessionUsage(cur.originId);
        bus.publish({
          type: "usage.updated",
          sessionId: cur.originId,
          data: { sessionId: cur.originId, ...u, tree: db.treeUsage(cur.originId) },
        });
      }
    }
    // A workspace session may have new file edits after the turn — nudge the Changes
    // rail to refetch. Only workspace-backed sessions have anything to show.
    if (db.getSessionRuntime(sessionId).workspace) {
      bus.publish({ type: "changes.updated", sessionId, data: { sessionId } });
    }
  }
}

async function executeTool(
  tools: ToolDef[],
  name: string,
  rawInput: unknown,
  ctx: ToolRunCtx,
): Promise<{ output: string; isError: boolean }> {
  const tool = tools.find((t) => t.name === name);
  if (!tool) return { output: `unknown tool: ${name}`, isError: true };
  let parsed: unknown;
  try {
    parsed = tool.schema.parse(rawInput);
  } catch (e) {
    return { output: `invalid input for ${name}: ${(e as Error).message}`, isError: true };
  }
  try {
    return { output: await tool.run(parsed, ctx), isError: false };
  } catch (e) {
    return { output: `${name} failed: ${(e as Error).message}`, isError: true };
  }
}

// ---- history assembly ------------------------------------------------------

/** The latest user turn's text — the message that triggered this run (skills lookup). */
function lastUserText(db: Db, sessionId: string): string {
  const users = db.threadFor(sessionId).filter((m) => m.role === "user");
  const last = users[users.length - 1];
  if (!last) return "";
  return last.parts
    .filter((p): p is Extract<Part, { type: "text" }> => p.type === "text")
    .map((p) => p.text)
    .join("\n");
}

/** Root→leaf thread mapped to Anthropic messages, excluding the pending message. */
function buildHistory(db: Db, sessionId: string, pendingId: string): LlmMessage[] {
  return db
    .threadFor(sessionId)
    .filter((m) => m.id !== pendingId)
    .flatMap(toLlmMessages);
}

/**
 * Expand @path references in the LAST user message into inlined file content
 * (in place). Scoped to the triggering message so we don't re-read files for the
 * whole history every turn.
 */
function inlineFileReferences(messages: LlmMessage[], workspace: string): void {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role !== "user") continue;
    for (const block of m.content) {
      if (block.type === "text") block.text = expandFileReferences(block.text, workspace);
    }
    return; // only the most recent user message
  }
}

function toLlmMessages(m: Message): LlmMessage[] {
  // System notes (harness-injected, e.g. subagent reports) replay as user-side text:
  // they are input TO the model, never words it said.
  if (m.role === "user" || m.role === "system") {
    const content: LlmContentBlock[] = m.parts
      .filter((p) => p.type === "text")
      .map((p) => ({ type: "text", text: (p as { text: string }).text }));
    return content.length ? [{ role: "user", content }] : [];
  }

  const assistant: LlmContentBlock[] = [];
  const results: LlmContentBlock[] = [];
  const resolved = new Set<string>();
  const requested = new Set<string>();
  for (const p of m.parts) {
    if (p.type === "text") {
      assistant.push({ type: "text", text: p.text });
    } else if (p.type === "reasoning") {
      // dropped on replay (see module header)
    } else if (p.type === "tool_call") {
      requested.add(p.id);
      assistant.push({ type: "tool_use", id: p.id, name: p.name, input: p.input });
    } else if (p.type === "tool_result") {
      resolved.add(p.callId);
      results.push({
        type: "tool_result",
        toolUseId: p.callId,
        content: stringifyOutput(p.output),
        isError: p.isError,
      });
    }
  }
  // Close any tool_use that never got a result (e.g. a crash) so the API accepts it.
  for (const id of requested) {
    if (!resolved.has(id)) {
      results.push({ type: "tool_result", toolUseId: id, content: "(interrupted)", isError: true });
    }
  }

  const out: LlmMessage[] = [];
  if (assistant.length) out.push({ role: "assistant", content: assistant });
  if (results.length) out.push({ role: "user", content: results });
  return out;
}

function stringifyOutput(output: unknown): string {
  return typeof output === "string" ? output : JSON.stringify(output);
}
