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
 *   - user message      → one user message of text blocks (+ image blocks, loaded
 *     base64 from ~/.bough/attachments; a lost attachment replays as placeholder text)
 *   - supervisor/worker → an assistant message (text + tool_use) followed, if it
 *     produced tool results, by a user message of tool_result blocks
 *   - reasoning parts   → DROPPED on replay. They're persisted for display, but we
 *     don't run extended thinking, so there are no signed thinking blocks to echo;
 *     re-sending them as plain text would only confuse the model.
 *   - any tool_use without a matching tool_result (e.g. a crash mid-tool) gets a
 *     synthetic error tool_result so the history stays valid for the API.
 *   - ask parts        → user-side text after the tool_results ("[ask] Q → the user
 *     answered: A"): the answer was the USER's input, and plain text can never
 *     re-block a replay the way re-raising the hold would.
 */
import type { Db } from "./db/db.ts";
import type { Bus } from "./bus.ts";
import type { ImagePart, Message, Part } from "./schema/parts.ts";
import {
  defaultTools,
  DONE_ACCEPTED,
  jsonSchema,
  type ToolDef,
  type ToolRunCtx,
} from "./tools/mod.ts";
import { runningIds } from "./tools/bash_bg.ts";
import { contextWindowFor, usageCostUsd } from "./pricing.ts";
import {
  clientFor,
  type Effort,
  EFFORTS,
  errName,
  isRetryable,
  type LlmClient,
  type LlmContentBlock,
  type LlmMessage,
  type RetryOpts,
} from "./supervisor/llm.ts";
import { checkpoint, finishTurn, startTurn } from "./supervisor/turns.ts";
import {
  adoptSubagent,
  joinSubagent,
  MAX_SUBAGENT_DEPTH,
  runSubagent,
  spawnSubagentDetached,
  startSubagent,
  subagentDepth,
} from "./subagent.ts";
import { type WorkflowCtx, workflowVerb } from "./workflow.ts";
import { clip } from "./text.ts";
import { maybeAutoTitle, type Titler } from "./supervisor/title.ts";
import {
  delegationHintNote,
  readAgentsFile,
  resolveSystemSections,
  runningSubagentsNote,
  scratchpadNote,
  workspaceNote,
} from "./supervisor/prompt.ts";
import { activeSkills } from "./supervisor/skills.ts";
import {
  hostReadRoot,
  normalizeWorkspace,
  type PreparedWorkspace,
  prepareWorkspace,
} from "./supervisor/workspace.ts";
import { activationsFor } from "./mcp/config.ts";
import { createLspBridge, lspAvailable, lspSection } from "./mcp/lsp.ts";
import { mcpManager } from "./mcp/manager.ts";
import { mcpSection } from "./mcp/prompt.ts";
import { mcpStatusFor } from "./mcp/status.ts";
import {
  attachImageFile,
  collectImageAttachments,
  expandFileReferences,
  imagePartToBlock,
} from "./server/files.ts";
import { publishArtifact } from "./server/artifacts.ts";
import { recall as recallSearch } from "./recall.ts";
import { expireAsks, raiseAsk } from "./asks.ts";
import { scheduleVerb } from "./schedules.ts";
import { stateVerb } from "./state.ts";

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
 * Thinking depth new turns run at ("" = provider default: request unchanged).
 * Starts from BOUGH_EFFORT and changes at runtime via PATCH /config, with the
 * same per-session pinning as the model (session.effort wins when set).
 */
let currentEffort = ((): Effort | "" => {
  const e = Deno.env.get("BOUGH_EFFORT") ?? "";
  return (EFFORTS as string[]).includes(e) ? (e as Effort) : "";
})();

export function activeEffort(): Effort | "" {
  return currentEffort;
}

export function setActiveEffort(effort: Effort | ""): void {
  currentEffort = effort;
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
}
export const MODELS: ModelRow[] = [
  { id: "claude-opus-4-8", label: "Opus 4.8", provider: "anthropic" },
  { id: "claude-opus-5", label: "Opus 5", provider: "anthropic" },
  { id: "claude-fable-5", label: "Fable 5", provider: "anthropic" },
  { id: "claude-sonnet-5", label: "Sonnet 5", provider: "anthropic" },
  { id: "claude-haiku-4-5", label: "Haiku 4.5", provider: "anthropic" },
  { id: "openai:gpt-5", label: "GPT-5 (OpenAI)", provider: "openai" },
  { id: "openai:gpt-5-mini", label: "GPT-5 mini (OpenAI)", provider: "openai" },
  { id: "openai/gpt-5", label: "GPT-5 (OpenRouter)", provider: "openrouter" },
  { id: "openai/gpt-oss-120b", label: "GPT-OSS 120B (OpenRouter)", provider: "openrouter" },
  { id: "google/gemini-2.5-pro", label: "Gemini 2.5 Pro (OpenRouter)", provider: "openrouter" },
  { id: "z-ai/glm-5.2", label: "GLM 5.2 (OpenRouter)", provider: "openrouter" },
  {
    id: "deepseek/deepseek-v4-flash",
    label: "DeepSeek V4 Flash (OpenRouter)",
    provider: "openrouter",
  },
  { id: "moonshotai/kimi-k3", label: "Kimi K3 (OpenRouter)", provider: "openrouter" },
];

const MAX_TOKENS = 64_000;

/**
 * Prewalk (/prewalk skill): the turn opens on the session's own (frontier) model
 * carrying the skill's plan-deeply instruction; the moment the first edit lands
 * (run_steps sets turn.everWrote) the loop swaps to this cheaper model for the
 * remaining rounds. The cheap model inherits the whole exploration trajectory
 * in-context — no plan document, no re-reads — with the planning instruction
 * pruned from its prompt and the frontier's signed thinking stripped (another
 * model's signatures don't validate).
 */
function prewalkModel(): string {
  return Deno.env.get("BOUGH_PREWALK_MODEL") ?? "claude-haiku-4-5";
}

/** `/prewalk` at a word boundary — mirrors skills.ts mentions(). */
const PREWALK_RE = /(^|\s)\/prewalk(\s|$)/;

/**
 * Usable prompt budget for a model: its catalog context window (pricing.ts)
 * minus the output reservation every round makes (MAX_TOKENS). The context
 * meter's "% left" is measured against this. Null when the window is unknown.
 */
export function usableContextLimit(model: string): number | null {
  const window = contextWindowFor(model);
  return window === null ? null : window - MAX_TOKENS;
}
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
/**
 * A literal "<stop/>" ending the text (possibly whitespace-padded / repeated):
 * the model sometimes EMITS the sentinel as text instead of calling the stop
 * tool, which used to render verbatim in transcripts and cost a nudge round.
 * Tolerant parse: strip it (loop control, not content — same rule as the stop
 * call itself) and honor it as a stop request. End-anchored on purpose so text
 * merely mentioning the token (code spans, docs) is never touched.
 */
const TRAILING_STOP_SENTINEL = /(?:\s*<stop\s*\/>)+\s*$/i;
/** Re-prompts before the harness gives up on an explicit stop (runaway brake). */
const MAX_STOP_NUDGES = 3;

// Turn-level recovery ring: how many times a turn survives a round whose inner
// withRetries window was exhausted by a transient failure, and how long it
// pauses before resuming. 2 × 60s rides out a multi-minute network flap while a
// truly dead network still fails the turn in a few minutes.
const TURN_RING_MAX = 2;
const TURN_RING_DELAY_MS = 60_000;
const STOP_NUDGE = "[harness] Your turn is still open — it only ends when you call the stop " +
  "tool. Continue if there is more to do, or call stop now (alone, no other output) if you " +
  "are finished.";
// A turn that ends with no text part shows the user nothing but collapsed tool
// calls — the agent looks mute. If the model tries to end (stop, or an accepted
// done-check) without having said anything, re-prompt once for a report.
// Asks for a CLOSING report, not merely "some text". The old wording ("you have
// written no user-visible text this turn") described the mute case only, and an
// agent that narrated on its way through would end with its last word being
// "Let me implement the changes:" over a raw tool dump. What the user needs at
// the end is the outcome, not the plan.
const REPORT_NUDGE = "[harness] Your turn is about to end and the last thing the user " +
  "can see is tool output — anything you wrote earlier was narration of work in " +
  "progress, not a conclusion. Close the turn now: say what you changed (name the " +
  "files), what you verified and how it came out, and anything you did NOT do or " +
  "left uncertain. A few lines is plenty; do not restate your plan or re-explain " +
  "the code. Then call stop in the same response.";

// Parallelism-claim honesty: text that claims concurrent/background execution
// when no parallel primitive (agent/spawn/bashBg — turn.ranParallel) ran this
// turn. Conservative on purpose: bare "background" is excluded (too common as a
// noun — "background info", "background: …"); missing a phrasing is fine, a
// false positive is not, and the no-primitive-ran condition already limits the
// blast radius to turns that did tool work while describing it as parallel.
const PARALLEL_CLAIM =
  /\b(?:in parallel|concurrent(?:ly)?|simultaneous(?:ly)?|in the background|backgrounded)\b/i;
const PARALLEL_CLAIM_NUDGE =
  "[harness] You described work as parallel/background, but no subagent or background " +
  "shell ran this turn — correct the description, or actually parallelize.";

const STOP_GATE_NUDGE = "[harness] You changed files this turn but never passed a " +
  "committed check — stop would end the turn with the work unverified. Either commit " +
  "a `check` that encodes the request's acceptance criteria and set done:true, or, " +
  "if these changes genuinely need no verification, call stop again to end anyway.";

/**
 * Kick off the supervisor turn for `sessionId`. Returns the placeholder message
 * plus the promise that resolves when the turn is fully done — the 202 path
 * discards it (fire-and-forget); tests await it.
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

/**
 * First-message text awaiting auto-title, keyed by session. Flushed at the
 * turn's first output (markFirstOutput): a turn that fails with nothing to
 * show must not name the session from its user message.
 */
const pendingTitles = new Map<string, string>();

/**
 * Map known auth/key failures to plain language with the fix at hand (^o is
 * the API-keys tab). Anything unrecognized keeps the raw SDK message.
 */
function friendlyTurnError(err: unknown, model: string): string {
  const msg = (err as Error)?.message ?? String(err);
  const provider = model.startsWith("openai:")
    ? "OpenAI"
    : model.includes("/")
    ? "OpenRouter"
    : "Anthropic";
  // Missing key: the Anthropic SDK's "Could not resolve authentication method…"
  // or llm.ts's "<ENV>_API_KEY is not set" for the bearer-token providers.
  if (/Could not resolve authentication method|apiKey or authToken|API_KEY is not set/i.test(msg)) {
    return `No ${provider} API key set — press ^o to add one.`;
  }
  // Key present but rejected (401 bodies from any of the three providers).
  if (/invalid x-api-key|authentication_error|Incorrect API key/i.test(msg)) {
    return `${provider} rejected the API key — press ^o to update it.`;
  }
  // Provider HTTP errors surface as "<provider>: <status> <body>", where the body
  // is often a multi-line escaped-JSON blob (e.g. Kimi/OpenRouter's tool-protocol
  // 400). Fold it to ONE human line so cards AND upward subagent reports stay
  // readable instead of dumping 6 lines of raw JSON.
  const http = /:\s*(\d{3})\s+([\s\S]+)$/.exec(msg);
  if (http) {
    const status = Number(http[1]);
    const body = http[2];
    if (/tool_calls|tool_call_id|must be followed by tool/i.test(body)) {
      return `${provider} rejected the tool-call formatting (${status}); a repaired retry usually clears it.`;
    }
    if (status >= 400) {
      const brief = body.replace(/\s+/g, " ").trim();
      return `${provider} error ${status}: ${
        brief.length > 120 ? brief.slice(0, 120) + "…" : brief
      }`;
    }
  }
  return msg;
}

/**
 * Cascade hooks fired when a session is interrupted — how a stop reaches work not
 * tied to the turn's own signal. subagent.ts registers one per DETACHED child so an
 * explicit interrupt of the spawner stops the whole subtree (a runaway detached
 * subagent is otherwise unstoppable). A normal turn end does NOT fire these, so
 * detached spawns still survive the turn ending — only an explicit stop cascades.
 */
const interruptHooks = new Map<string, Set<() => void>>();

/** Register a cascade hook for `sessionId`; returns an unregister thunk. */
export function onInterrupt(sessionId: string, cb: () => void): () => void {
  let set = interruptHooks.get(sessionId);
  if (!set) interruptHooks.set(sessionId, set = new Set());
  set.add(cb);
  return () => {
    set!.delete(cb);
    if (set!.size === 0) interruptHooks.delete(sessionId);
  };
}

/** True if a turn is currently running for this session. */
export function isTurnRunning(sessionId: string): boolean {
  return running.has(sessionId);
}

/**
 * Interrupt the session's in-flight turn AND cascade to its detached subagents
 * (interrupt hooks). Aborts the current LLM request and signals the loop to stop
 * after the current step. Fires cascade hooks even when the session itself is idle
 * (its turn ended but a detached child runs on). Returns false only if there was
 * nothing to stop.
 */
export function interruptTurn(sessionId: string): boolean {
  const c = running.get(sessionId);
  if (c) c.abort();
  const hooks = interruptHooks.get(sessionId);
  if (hooks) {
    for (const h of [...hooks]) {
      try {
        h();
      } catch { /* a child already gone is fine */ }
    }
  }
  return !!c || (hooks?.size ?? 0) > 0;
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
  // Image @refs become attachment-backed image parts NOW (the file is copied to
  // ~/.bough/attachments, so the message replays after the original moves);
  // text @refs keep inlining lazily at replay time (inlineFileReferences).
  // Relative refs resolve against the session's host-side view: the session's
  // host worktree when one exists, else the workspace itself.
  const ws = ctx.db.getSessionRuntime(sessionId).workspace;
  const images = collectImageAttachments(
    text,
    ws ? hostReadRoot(sessionId, normalizeWorkspace(ws)) : null,
  );
  const userMessage: Message = {
    id: crypto.randomUUID(),
    sessionId,
    role: "user",
    parts: [{ type: "text", text }, ...images],
    pending: false,
    createdAt: Date.now(),
  };
  ctx.db.createMessage(userMessage);
  ctx.bus.publish({ type: "message.started", sessionId, data: userMessage });
  // Name an untitled session from its first message (title worker) — but only
  // once its turn produces output (see markFirstOutput): a failed "hello" must
  // not leave the session titled "Hello".
  pendingTitles.set(sessionId, text);
  // One turn per session: if one is already running, the message is persisted and
  // shown now, and it STEERS — the live turn yields at its next round boundary and
  // the follow-up turn (which sees this message) starts immediately. Clients that
  // want plain queueing instead hold the message until the turn finishes.
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
/**
 * The production WorkflowCtx: agent() calls become real subagent sessions
 * branched off `sessionId`, anchored to `spawnerMessageId` on the map. Used by
 * the turn runner's workflow.* wiring AND the REST workflow routes; the runner
 * observes the RUN's abort signal (a workflow outlives the turn that started it).
 */
export function workflowCtxFor(
  ctx: TurnCtx,
  sessionId: string,
  spawnerMessageId: string,
  model?: string,
): WorkflowCtx {
  return {
    db: ctx.db,
    bus: ctx.bus,
    runner: async (call, signal, onSpawned) => {
      const h = await startSubagent(ctx, {
        spawnerId: sessionId,
        spawnerMessageId,
        model: call.model ?? model,
        signal,
        capsExempt: true,
      }, call.prompt);
      onSpawned(h.sessionId);
      // Stop cascades into a subagent whose turn is still running, same
      // containment as runSubagent's blocking mode.
      const onAbort = () => {
        if (isTurnRunning(h.sessionId)) interruptTurn(h.sessionId);
      };
      signal.addEventListener("abort", onAbort, { once: true });
      try {
        const r = await h.result;
        if (!r.ok) {
          throw new Error(`subagent ${r.status}: ${clip(r.report || "(no report)", 400)}`);
        }
        return r.report;
      } finally {
        signal.removeEventListener("abort", onAbort);
      }
    },
    notify: (sid, text) => postSystemNote(ctx, sid, text),
  };
}

export function postSystemNote(
  ctx: TurnCtx,
  sessionId: string,
  text: string,
  // Extra parts riding along with the note — image() attaches a picture this way,
  // which is the only route a picture has into the model (a tool_result is text).
  extra: Part[] = [],
): void {
  const msg: Message = {
    id: crypto.randomUUID(),
    sessionId,
    role: "system",
    parts: [{ type: "text", text }, ...extra],
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
  // process-global default (what new sessions start on). Mutable: a prewalk
  // handoff swaps it (and the client) mid-turn at a round boundary.
  let model = ctx.model ?? db.getSession(sessionId)?.model ?? activeModel();
  // Thinking depth: the session's pin wins over the global default; "" = unset
  // (the request carries no thinking/effort fields at all).
  const effort = (db.getSession(sessionId)?.effort as Effort | undefined) ??
    (activeEffort() || undefined);
  const retryOpts: RetryOpts = {
    onRetry: ({ attempt, maxAttempts, error, delayMs }) => {
      // A retried round re-streams from the top: tell the UI to drop this
      // message's partial streamed text, and say what's happening.
      bus.publish({ type: "message.retry", sessionId, data: { messageId } });
      const reason = error.message.replace(/\s+/g, " ").slice(0, 80);
      bus.publish({
        type: "session.activity",
        sessionId,
        data: {
          text: `⟳ LLM failed (${reason}) — retry ${attempt + 1}/${maxAttempts} in ${
            Math.ceil(delayMs / 1000)
          }s`,
        },
      });
    },
  };
  let llm = ctx.llm ?? clientFor(model, retryOpts);
  const tools = ctx.tools ?? defaultTools;
  // Newest round's input_tokens ≈ the live context size; output accumulates, and
  // input accumulates too (cost: every round re-sends the whole thread).
  let contextTokens = 0;
  let outputTokens = 0;
  let inputTokens = 0;
  // Last round's cached prompt share + finish time — the cache-warmth clock the
  // tree view decays against (Anthropic's 5-min sliding TTL starts here).
  let cachedTokens = 0;
  let cacheReadTokens = 0; // cumulative this turn — reads bill ~0.1x
  let cacheWriteTokens = 0; // cumulative this turn — writes bill ~1.25x
  let lastLlmAt = 0;
  // Dollars this turn, priced per round at the round's model (pricing.ts) — the
  // session model can change mid-session, so cumulative token totals can't be
  // priced after the fact. Models missing from the catalog contribute 0.
  let costUsd = 0;

  const turn = startTurn(db, sessionId, messageId);
  // Time-to-first-output metric: stamp the moment ANYTHING from this turn becomes
  // visible to the user — the first streamed delta or the first finalized part,
  // whichever lands first (a tool-only round has no text deltas).
  let sawOutput = false;
  const markFirstOutput = () => {
    if (sawOutput) return;
    sawOutput = true;
    db.setTurnFirstOutput(turn.id, Date.now());
    // Proof of life: fire the deferred auto-title now (see startUserTurn).
    const titleText = pendingTitles.get(sessionId);
    if (titleText !== undefined) {
      pendingTitles.delete(sessionId);
      maybeAutoTitle({ db, bus, titler: ctx.titler }, sessionId, titleText);
    }
  };
  const parts: Part[] = [];
  // Set right before the message's final write. A late ask() settle (program
  // timeout, expire at turn end) must not append into a finished message — that
  // would flip it pending again and strand the UI on a turn that already ended.
  let finalized = false;
  const append = (part: Part) => {
    markFirstOutput();
    parts.push(part);
    db.updateMessage(messageId, parts, true);
    bus.publish({ type: "message.part", sessionId, data: { messageId, part } });
  };

  try {
    // Resolve the workspace and (if sandboxed) set up snapshots + the snapshot dir once.
    const prepared: PreparedWorkspace = await prepareWorkspace(db, sessionId, ctx.workspace);
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
    // The tool_use id currently executing — rebound each iteration so ctx.onLog
    // attributes streamed lines to the right tool_call part.
    let currentCallId = "";
    const toolCtx: ToolRunCtx = {
      workspace: prepared.cwd,
      sessionId,
      // Interrupt reaches INTO a running tool: run_steps terminates its program's
      // worker and bash kills its child — stop means stop, not "after this step".
      signal,
      sandbox: prepared.sandboxed
        ? {
          sessionDir: prepared.sessionDir,
          scratchDir: prepared.scratchDir,
        }
        : undefined,
      // Per-turn harness state: run_steps commits the CHECK here (SPEC §5 gating).
      // Multi-rule requests (≥2 numbered rules) additionally carry their text so
      // the done-gate can replay the spec once at the decisive moment — weak
      // models drop prose sub-clauses by then (bench: refactor-behavior task).
      turn: ((): { requestText?: string } => {
        // `message` is the pending supervisor placeholder — the request lives in
        // the thread's last USER message.
        const users = db.threadFor(sessionId).filter((m) => m.role === "user");
        const text = (users.at(-1)?.parts ?? [])
          .filter((p): p is Extract<Part, { type: "text" }> => p.type === "text")
          .map((p) => p.text).join("\n");
        const numbered = (text.match(/^\s*\d+[.)]\s/gm) ?? []).length;
        return numbered >= 2 ? { requestText: text } : {};
      })(),
      // Management-plane visibility, always on: the program can ask what MCP state
      // this session sees without shelling curl at the loopback API. Read-only —
      // tool calls still enter through the granted mcp() bridge below.
      mcpStatus: () => Promise.resolve(mcpStatusFor(sessionId)),
      // Completion notes from background shells (bash_bg.ts): a finished job wakes
      // the session with a one-line note, so the model never polls. Same wake path
      // as a subagent's report — starts a turn if idle, else rides the queued-drain.
      notify: (text) => postSystemNote(ctx, sessionId, text),
      // Live program output: each console.* line becomes a tool.log event the TUI
      // renders under the running call. Display-only — the model's tool_result
      // still carries the joined logs when the program finishes. The executing
      // call's id is rebound per tool_use below (tools run sequentially).
      onLog: (line) => {
        markFirstOutput();
        bus.publish({
          type: "tool.log",
          sessionId,
          data: { messageId, callId: currentCallId, line },
        });
      },
    };
    // Recall: semantic search over all past conversations (recall.ts), host-side —
    // the sandbox never sees the DB or the embedder. Lazily indexes as it's used.
    toolCtx.recall = (query, k) => recallSearch(db, query, k);
    // Schedules: the model manages recurring runs through the SAME validated code
    // path as the REST CRUD (schedules.ts). schedule.add() without a workspace
    // defaults to this session's persisted workspace.
    // Durable notes (state.ts): scoped to the lineage's ROOT session so a fork,
    // a compaction child and a subagent all read the same store — the store is
    // there precisely for facts the transcript will not keep.
    const stateRoot = db.ancestorChain(sessionId)[0]?.id ?? sessionId;
    toolCtx.state = { call: (verb, args) => Promise.resolve(stateVerb(db, stateRoot, verb, args)) };
    toolCtx.schedule = {
      call: (verb, args) => scheduleVerb(db, verb, args, db.getSessionRuntime(sessionId).workspace),
    };
    // image(): the program has no eyes — this is how a screenshot or a rendered
    // chart it just produced actually reaches the model. Host-side we copy the
    // file into ~/.bough/attachments (so the message replays after the file moves)
    // and post it as a system note carrying an image part. History is assembled
    // once per turn, so the picture lands on the NEXT turn — the same wake path a
    // background shell's completion note uses; the confirmation says so.
    // Synchronous now that there is no overlay to read the bytes out of: the
    // attach is a plain host copyFile, so the only Promise here is the return.
    toolCtx.image = (path, note) => {
      const rel = path.startsWith("/")
        ? path
        : path.startsWith("~/")
        ? `${Deno.env.get("HOME") ?? "."}${path.slice(1)}`
        : `${prepared.cwd.replace(/\/+$/, "")}/${path}`;
      // Plain host read: tools write real files now, so a screenshot the program
      // just rendered is simply there.
      const part: ImagePart | null = attachImageFile(rel, path);
      if (!part) {
        throw new Error(
          `image(): cannot attach ${path} — missing, unreadable, not a png/jpg/gif/webp, or over 5MB`,
        );
      }
      postSystemNote(ctx, sessionId, `[image] ${path}${note ? ` — ${note}` : ""}`, [part]);
      return Promise.resolve(`attached ${path} (${part.size} bytes); you will see it next turn`);
    };
    // Artifacts: the program publishes a file for browser viewing; we host it on the
    // server (server/artifacts.ts). The TUI lists it via GET /sessions/:id/artifacts.
    toolCtx.artifact = async (name, content) => {
      return await publishArtifact(sessionId, name, content);
    };
    // ask(): park the program on a question to the human (asks.ts — a hold-and-ask
    // pattern). The settled Q/A is appended to THIS message as an
    // "ask" part, so the transcript keeps it and replay renders it as plain text
    // (toLlmMessages) — replay never re-blocks.
    toolCtx.ask = async (question, opts) => {
      const options = opts?.options?.map((o) => String(o).trim()).filter(Boolean);
      const { record, answer } = raiseAsk(
        bus,
        { sessionId, messageId, question, ...(options?.length ? { options } : {}) },
        signal,
      );
      const askPart = (status: "answered" | "declined" | "interrupted", ans?: string): Part => ({
        type: "ask",
        id: record.id,
        question,
        ...(options?.length ? { options } : {}),
        status,
        ...(ans !== undefined ? { answer: ans } : {}),
      });
      try {
        const ans = await answer;
        if (!finalized) append(askPart("answered", ans));
        return ans;
      } catch (err) {
        if (!finalized) {
          append(askPart(record.status === "declined" ? "declined" : "interrupted"));
        }
        throw err;
      }
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
    const triggerText = lastUserText(db, sessionId);
    const skills = activeSkills(triggerText);
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
        run: async (task) => {
          if (toolCtx.turn) toolCtx.turn.ranParallel = true;
          return await runSubagent(subCtx, sctx, task);
        },
        // Kept for compatibility with programs (and prompts) that still call it:
        // subagents share this session's checkout, so it just says so.
        adopt: (subId) => adoptSubagent(ctx, sessionId, subId),
        ...(isSub ? {} : {
          spawn: (task) => {
            if (toolCtx.turn) toolCtx.turn.ranParallel = true;
            return spawnSubagentDetached(subCtx, sctx, task);
          },
          join: (subId) => joinSubagent(sctx, subId),
        }),
      };
      // Workflows (root sessions only): scripted orchestration over the same
      // subagent machinery. A run is DETACHED from this turn — its runner uses
      // the RUN's abort signal, not the turn's, so ending the turn doesn't kill
      // the run; its finished report arrives as a system note.
      if (!isSub) {
        const wctx = workflowCtxFor(subCtx, sessionId, messageId, model);
        toolCtx.workflow = { call: (verb, args) => workflowVerb(wctx, sessionId, verb, args) };
      }
    }

    const messages = buildHistory(db, sessionId, messageId);
    // Anything queued/steered before this point is already in the history we just
    // built — clear the flags so it isn't re-delivered by a spurious follow-up
    // turn or an instant yield below.
    queued.delete(sessionId);
    steering.delete(sessionId);
    // Inline any @path references in the triggering message so the model sees the
    // file content, not just the name. Only for workspace sessions (files exist).
    // hostView is the session's host worktree (== cwd).
    if (prepared.sandboxed) inlineFileReferences(messages, prepared.hostView);
    // Project rules: a global ~/.bough/AGENTS.md plus the workspace root's AGENTS.md are
    // authoritative for build/test commands, conventions, and what "done" means — inject them.
    const agents = prepared.sandboxed ? await readAgentsFile(prepared.hostView) : null;
    // MCP: connect the turn's granted servers (grantedMcp, resolved above) so the
    // prompt can list real tools; a server that fails to connect is named
    // UNAVAILABLE instead of vanishing. Subagent turns connect their inherited
    // grant here too — under their own session id, so gating and workspace stay
    // scoped to the subagent.
    let mcpNote = "";
    if (grantedMcp.length > 0) {
      const catalog = await mcpManager().ensure(sessionId, grantedMcp, {
        workspace: prepared.hostView,
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
    // LSP: always-on when the leta CLI is installed — symbol navigation is a
    // core capability, not a skill grant. Nothing spawns until the program's
    // first lsp.* call (see mcp/lsp.ts for the host-side confinement trade-off).
    let lspNote = "";
    if (lspAvailable()) {
      toolCtx.lsp = createLspBridge({
        workspace: prepared.hostView,
        sandbox: toolCtx.sandbox,
      });
      lspNote = lspSection();
    }
    // Supervisor prompt sections, resolved for this session's optional promptDir
    // override (bough exec --prompt-dir) — read per turn, so a pinned variant needs
    // no server restart. Undefined override = the process default sections.
    const {
      SYSTEM,
      SYSTEM_DELEGATION,
      SYSTEM_DELEGATION_NESTED,
      SYSTEM_SUBAGENT,
    } = resolveSystemSections(db.getSession(sessionId)?.promptDir ?? undefined);
    // No ship()/pr() host functions: bash runs in the user's own checkout, so
    // committing and pushing is `git commit` / `git push` / `gh pr create` like any
    // other command. The pair existed only to carry work out of the copy-on-write
    // overlay that used to sit under the tools (see tools/bash.ts).
    // System prompt in two tiers for prompt caching — llm.ts places a cache
    // breakpoint after each (see anthropicClient's caching comment):
    //
    //   STABLE (`system`) — must be byte-identical across sessions and turns so
    //   the provider cache shares it machine-wide; only text free of per-session
    //   facts belongs here. The ship/subagent/delegation sections are constant
    //   TEXT with conditional PRESENCE: they split the cache into a handful of
    //   tiers (root+ship, root, subagent, depth-capped subagent), each still
    //   shared by every session of that tier — acceptable. lspSection() is
    //   constant text gated on a per-machine binary check, so it rides the
    //   stable tier (moved up from after the MCP section — it's a self-contained
    //   "##" section nothing references by position).
    //
    //   VOLATILE (`systemVolatile`) — everything carrying per-session/per-turn
    //   facts: running-subagent ids, workspace + scratchpad paths, AGENTS.md
    //   content, the MCP catalog, invoked skills. Usually stable across turns
    //   within a session, so its own breakpoint still pays; it must always come
    //   AFTER the stable tier or it poisons the shared prefix.
    //
    // Relative order within each tier is unchanged from the pre-split prompt.
    const system = SYSTEM +
      (isSub ? SYSTEM_SUBAGENT : "") +
      (mayDelegate ? (isSub ? SYSTEM_DELEGATION_NESTED : SYSTEM_DELEGATION) : "") +
      lspNote;
    const volatileBase =
      // Delegation fit gate: a decomposable-shaped request gets the decision rule
      // + spawn() code shape injected once (root sessions only — spawn exists
      // there). Cohesive requests see a byte-identical prompt (see prompt.ts).
      // Per-turn text, so it lives in the volatile tier — leading it, which
      // keeps its pre-split position right after SYSTEM_DELEGATION.
      (mayDelegate && !isSub ? delegationHintNote(triggerText) : "") +
      (mayDelegate && !isSub
        ? runningSubagentsNote(
          db.listSessions().filter((s) =>
            s.kind === "subagent" && s.originId === sessionId && isTurnRunning(s.id)
          ),
        )
        : "") +
      workspaceNote(prepared.cwd) +
      (prepared.sandboxed ? scratchpadNote(prepared.scratchDir) : "") +
      (agents ?? "") +
      mcpNote;
    let systemVolatile = volatileBase + skills.sections;
    // Prewalk: armed when the /prewalk skill resolved into the prompt and the
    // cheap target differs from this turn's model. `volatile` is the prompt to
    // adopt at handoff — the same build minus the prewalk section, so the cheap
    // model never sees the planning instruction it didn't follow.
    const prewalkTarget = prewalkModel();
    const prewalk = skills.sections.includes("Active skill: /prewalk") && prewalkTarget !== model
      ? {
        target: prewalkTarget,
        volatile: volatileBase +
          activeSkills(triggerText.replace(PREWALK_RE, " ")).sections,
      }
      : null;
    let prewalkDone = false;
    // Tool defs precede system in the API's cache order, so they're part of the
    // shared prefix: defaultTools + STOP_TOOL is process-constant and jsonSchema
    // is deterministic, keeping the array byte-stable across sessions. A
    // per-session tool would split the cache — grant capabilities via host
    // functions inside run_steps (prompt sections), never via new tool defs.
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
    let reportNudges = 0;
    let ringRetries = 0; // turn-level recovery ring (see TURN_RING_MAX)
    // Any real tool ran this turn — arms the parallelism-honesty gate below.
    let ranTools = false;
    // Stop-gate: a turn that wrote files must exit through the check gate (done)
    // or explicitly decline it twice — stop is not a side door around verification.
    let anyDoneAccepted = false;
    let stopGateNudged = false;
    const stopGateBlocks = (): boolean => {
      if (anyDoneAccepted || stopGateNudged || !toolCtx.turn?.everWrote) return false;
      stopGateNudged = true;
      return true;
    };
    // Parallelism honesty: fires only when the turn did tool work (a pure
    // conversation turn can mention these words legitimately — advice, not a
    // claim about work it just did), no parallel primitive ran, and the turn's
    // text claims otherwise. One corrective nudge.
    let honestyNudged = false;
    const honestyGateNudge = (): string | null => {
      if (honestyNudged || !ranTools || toolCtx.turn?.ranParallel) return null;
      const said = parts
        .filter((p): p is Extract<Part, { type: "text" }> => p.type === "text")
        .map((p) => p.text).join("\n");
      if (!PARALLEL_CLAIM.test(said)) return null;
      honestyNudged = true;
      return PARALLEL_CLAIM_NUDGE;
    };
    /** End-gates, checked when the turn is about to end having said something:
     * each is one-shot and returns its nudge text, or null to let the end stand. */
    const endGateNudge = (): string | null => honestyGateNudge();
    /**
     * What the turn DID, rendered by the harness rather than asked of the model.
     *
     * A nudge can only request a summary; this states one. The files written and
     * the committed check's exit code are already known here, and they are the two
     * things a reader actually needs to trust the turn — so they are reported even
     * when the model ends on narration, contradicts itself, or says nothing. Prose
     * is dropped on replay, so this costs no context: the same facts are already in
     * the tool results the model can see.
     */
    let footerWritten = false;
    const appendFooter = () => {
      if (footerWritten) return;
      const t = toolCtx.turn;
      const files = t?.wroteFiles ?? [];
      const verdict = t?.checkVerdict;
      // Nothing changed and nothing was verified — a question answered, say. The
      // model's own text is the whole story there; a footer would just be noise.
      if (!files.length && !verdict) return;
      footerWritten = true;
      const lines: string[] = [];
      if (files.length) {
        lines.push(`**changed** ${files.length} file${files.length === 1 ? "" : "s"}`);
        for (const f of files) lines.push(`- \`${f}\``);
      } else {
        lines.push("**changed** nothing");
      }
      if (verdict) {
        lines.push(
          `\n**check** \`${verdict.cmd}\` → ${
            verdict.exit === 0 ? "passed" : `FAILED (exit ${verdict.exit})`
          }`,
        );
      } else {
        lines.push("\n**check** none committed — nothing was verified");
      }
      append({ type: "prose", text: lines.join("\n") });
    };
    // Last resort against a mute turn end: a round with tools forbidden
    // (toolChoice "none"), which reliably yields plain text where a second nudge
    // would just get another empty-thinking + stop.
    let forceText = false;
    /**
     * Has the model written a CLOSING summary — text the user sees after the work,
     * not narration from the middle of it?
     *
     * This used to be `parts.some(p => p.type === "text")`, which asked "was there
     * ever any text" and so was satisfied by mid-turn narration like "Let me
     * implement the changes:". The effect was backwards: the more an agent
     * explained itself as it worked, the more reliably its turn ended on a raw
     * tool_result — one observed turn closed on an `rg` match dump with no summary
     * at all, because it had narrated early. Only text after the last tool call
     * counts.
     */
    const saidSomething = () => {
      const lastTool = parts.findLastIndex((p) => p.type === "tool_call");
      return parts.slice(lastTool + 1).some((p) => p.type === "text");
    };
    for (let round = 0;; round++) {
      if (signal?.aborted) throw new InterruptedError();
      // A user message steered in mid-turn: yield here (a clean round boundary —
      // every tool_use already has its tool_result) and let the follow-up turn
      // continue with the new message in history.
      if (steering.has(sessionId)) break;
      // Prewalk handoff: the first landed edit is the swap point — from here the
      // cheap model continues the same in-context trajectory.
      if (prewalk && !prewalkDone && toolCtx.turn?.everWrote) {
        prewalkDone = true;
        model = prewalk.target;
        if (!ctx.llm) llm = clientFor(model, retryOpts);
        systemVolatile = prewalk.volatile;
        stripReasoning(messages);
        bus.publish({
          type: "session.activity",
          sessionId,
          data: { text: `⇄ prewalk: first edit landed — handing off to ${model}` },
        });
      }
      // Outer recovery ring: when a round exhausts withRetries' ~30s window on a
      // transient failure (connection drop, provider blip), pause and resume the
      // turn instead of killing it — a bench trial died with all its work intact
      // because 30s of network flap outlived the inner retries. Capped per turn;
      // non-retryable errors and aborts still fail/stop immediately.
      let result: Awaited<ReturnType<typeof llm.run>>;
      for (;;) {
        try {
          result = await llm.run(
            {
              model,
              system,
              systemVolatile,
              maxTokens: MAX_TOKENS,
              messages,
              tools: toolDefs,
              ...(forceText ? { toolChoice: "none" as const } : {}),
              ...(effort ? { effort } : {}),
            },
            (delta) => {
              markFirstOutput();
              bus.publish({ type: "message.delta", sessionId, data: { messageId, delta } });
            },
            signal,
          );
          break;
        } catch (err) {
          if (ringRetries >= TURN_RING_MAX || signal?.aborted || !isRetryable(err)) throw err;
          ringRetries++;
          // A retried round re-streams from the top — drop the partial stream.
          bus.publish({ type: "message.retry", sessionId, data: { messageId } });
          const reason = (err as Error).message?.replace(/\s+/g, " ").slice(0, 80);
          bus.publish({
            type: "session.activity",
            sessionId,
            data: {
              text: `⟳ provider unreachable (${reason}) — pausing ${
                TURN_RING_DELAY_MS / 1000
              }s, then resuming the turn (${ringRetries}/${TURN_RING_MAX})`,
            },
          });
          await new Promise<void>((resolve, reject) => {
            const timer = setTimeout(() => {
              signal?.removeEventListener("abort", onAbort);
              resolve();
            }, TURN_RING_DELAY_MS);
            const onAbort = () => {
              clearTimeout(timer);
              reject(new InterruptedError());
            };
            signal?.addEventListener("abort", onAbort, { once: true });
          });
        }
      }

      if (result.usage) {
        contextTokens = result.usage.inputTokens; // last round = current context size
        outputTokens += result.usage.outputTokens;
        inputTokens += result.usage.inputTokens;
        cachedTokens = (result.usage.cacheReadTokens ?? 0) +
          (result.usage.cacheCreationTokens ?? 0);
        cacheReadTokens += result.usage.cacheReadTokens ?? 0;
        cacheWriteTokens += result.usage.cacheCreationTokens ?? 0;
        costUsd += usageCostUsd(model, result.usage) ?? 0;
        lastLlmAt = Date.now();
      }

      // The stop call is loop control, not content: honor it, but never persist
      // or replay it — the thread and future prompts must not carry it.
      let stopRequested = false;
      const assistant: LlmContentBlock[] = [];
      for (const block of result.content) {
        if (block.type === "text") {
          // Emitted-sentinel stop (see TRAILING_STOP_SENTINEL): strip it from
          // what is stored/replayed and treat it as the stop call it meant.
          let text = block.text;
          if (TRAILING_STOP_SENTINEL.test(text)) {
            stopRequested = true;
            text = text.replace(TRAILING_STOP_SENTINEL, "");
          }
          if (text) {
            append({ type: "text", text });
            assistant.push({ type: "text", text });
          }
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

      // The forced text round is the turn's last word: whatever it said (text was
      // appended above; tools were forbidden) the turn ends here.
      if (forceText) break;

      const toolUses = result.content.filter(
        (b) => b.type === "tool_use" && b.name !== STOP_NAME,
      );
      if (toolUses.length > 0) {
        ranTools = true;
        const toolResults: LlmContentBlock[] = [];
        let doneAccepted = false;
        for (const tu of toolUses) {
          if (tu.type !== "tool_use") continue;
          // Don't start new tools once interrupted — stop before side effects.
          if (signal?.aborted) throw new InterruptedError();
          // Rebind the streaming attribution to this call, then run it.
          currentCallId = tu.id;
          const { output, isError } = await executeTool(tools, tu.name, tu.input, toolCtx);
          // An abort mid-tool means this result is a stopped run, not a completed
          // one — mark it so the UI renders ⏹ instead of ✓ over the partial output.
          const wasInterrupted = signal?.aborted === true;
          append({
            type: "tool_result",
            callId: tu.id,
            output,
            isError,
            ...(wasInterrupted ? { interrupted: true } : {}),
          });
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
            anyDoneAccepted = true;
          }
        }
        // Ending mute (accepted done / stop with no text this turn): ask for the
        // report INSIDE the tool_result message — Claude 5 answers an inline
        // nudge with text far more reliably than one sent as a separate message
        // (it tends to reply with empty thinking + stop). If even the nudge
        // round ends mute, escalate to a forced text-only round.
        if (stopRequested && !doneAccepted && stopGateBlocks()) {
          toolResults.push({ type: "text", text: STOP_GATE_NUDGE });
          messages.push({ role: "user", content: toolResults });
          continue;
        }
        const wantsEnd = doneAccepted || stopRequested;
        if (wantsEnd && !saidSomething()) {
          if (reportNudges < 1) {
            reportNudges++;
            toolResults.push({ type: "text", text: REPORT_NUDGE });
            messages.push({ role: "user", content: toolResults });
          } else {
            messages.push({ role: "user", content: toolResults });
            forceText = true;
          }
          continue;
        }
        if (wantsEnd) {
          const gate = endGateNudge();
          if (gate) {
            toolResults.push({ type: "text", text: gate });
            messages.push({ role: "user", content: toolResults });
            continue;
          }
        }
        messages.push({ role: "user", content: toolResults });
        if (wantsEnd) break;
        continue;
      }

      // No real tool calls this round: only an explicit stop ends the turn.
      if (stopRequested) {
        if (stopGateBlocks()) {
          messages.push({ role: "user", content: [{ type: "text", text: STOP_GATE_NUDGE }] });
          continue;
        }
        if (saidSomething()) {
          const gate = endGateNudge();
          if (gate) {
            messages.push({ role: "user", content: [{ type: "text", text: gate }] });
            continue;
          }
          break;
        }
        if (reportNudges < 1) {
          reportNudges++;
          messages.push({ role: "user", content: [{ type: "text", text: REPORT_NUDGE }] });
          continue;
        }
        // The nudge failed (typically an empty-thinking + stop). Drop that
        // reasoning-only assistant tail — ending the prompt on a thinking
        // prefill is invalid — and force one text-only round.
        const tail = messages.at(-1);
        if (
          tail?.role === "assistant" && tail.content.every((b) => b.type === "reasoning")
        ) {
          messages.pop();
        }
        forceText = true;
        continue;
      }
      // Trailed off without stop — re-prompt (in-memory only, never persisted),
      // with a cap so a stop-incapable model can't loop the API forever.
      if (nudges >= MAX_STOP_NUDGES) break;
      nudges++;
      messages.push({ role: "user", content: [{ type: "text", text: STOP_NUDGE }] });
    }

    // Placed after the loop so every normal exit gets it — accepted done, an
    // explicit stop, a mid-turn steer, or the stop-nudge cap. The catch path below
    // has its own ⏹/⚠︎ marker and deliberately does not.
    appendFooter();

    finalized = true;
    db.updateMessage(messageId, parts, false);
    finishTurn(db, turn.id, "done");
    bus.publish({ type: "message.finished", sessionId, data: { messageId } });
    bus.publish({ type: "turn.finished", sessionId, data: { sessionId, status: "done" } });
  } catch (err) {
    finalized = true;
    // An interrupt (explicit abort, or the SDK's abort error) ends the turn
    // cleanly with a marker, not a failure — the user asked it to stop.
    const interrupted = err instanceof InterruptedError || signal?.aborted ||
      errName(err) === "APIUserAbortError" || errName(err) === "AbortError";
    // Background shells are detached on purpose — say which ones outlive the stop.
    const survivors = interrupted ? runningIds(sessionId) : [];
    const note: Part = interrupted
      ? {
        type: "text",
        text: "⏹ Stopped." + (survivors.length
          ? `\n${survivors.join(", ")} still running — ${
            survivors.length === 1 ? "it survives" : "they survive"
          } the interrupt`
          : ""),
      }
      : { type: "text", text: `⚠︎ Turn failed: ${friendlyTurnError(err, model)}` };
    parts.push(note);
    db.updateMessage(messageId, parts, false);
    const status = interrupted ? "interrupted" : "error";
    // The UI must never know more than the server log does.
    if (!interrupted) console.error(`turn failed [${sessionId}]:`, err);
    finishTurn(db, turn.id, status);
    bus.publish({ type: "message.part", sessionId, data: { messageId, part: note } });
    bus.publish({ type: "message.finished", sessionId, data: { messageId } });
    bus.publish({ type: "turn.finished", sessionId, data: { sessionId, status } });
  } finally {
    // A question the turn never got answered (program timed out around it, or the
    // turn died some other way) must not haunt the TUI as a live hold.
    expireAsks(sessionId);
    // Persist token usage for the context meter and announce it (usage.updated).
    // contextTokens stays 0 for a stream that errored before its first usage report.
    if (contextTokens > 0 || outputTokens > 0) {
      const prev = db.sessionUsage(sessionId);
      const totals = {
        outputTokens: prev.outputTokens + outputTokens,
        inputTokens: prev.inputTokens + inputTokens,
        costUsd: prev.costUsd + costUsd,
      };
      db.setSessionUsage(
        sessionId,
        contextTokens,
        totals.outputTokens,
        totals.inputTokens,
        totals.costUsd,
      );
      if (lastLlmAt > 0) {
        db.setSessionCache(sessionId, cachedTokens, lastLlmAt, cacheReadTokens, cacheWriteTokens);
      }
      bus.publish({
        type: "usage.updated",
        sessionId,
        data: {
          sessionId,
          contextTokens,
          contextLimit: usableContextLimit(model),
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
          data: {
            sessionId: cur.originId,
            ...u,
            contextLimit: usableContextLimit(db.getSession(cur.originId)?.model ?? activeModel()),
            tree: db.treeUsage(cur.originId),
          },
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

/**
 * Drop reasoning blocks from the in-memory exchange (prewalk handoff): the
 * frontier's signed thinking fails signature validation under the swapped model,
 * so cross-model replay drops reasoning exactly as cross-turn replay does
 * (toLlmMessages). An assistant message left empty vanishes with its thinking.
 */
function stripReasoning(messages: LlmMessage[]): void {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role !== "assistant") continue;
    m.content = m.content.filter((b) => b.type !== "reasoning");
    if (m.content.length === 0) messages.splice(i, 1);
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
    // Image parts load from their attachment file here (base64); a missing file
    // degrades to a text placeholder inside imagePartToBlock, never a crash.
    const content: LlmContentBlock[] = m.parts.flatMap((p): LlmContentBlock[] =>
      p.type === "text"
        ? [{ type: "text", text: p.text }]
        : p.type === "image"
        ? [imagePartToBlock(p)]
        : []
    );
    return content.length ? [{ role: "user", content }] : [];
  }

  const assistant: LlmContentBlock[] = [];
  const results: LlmContentBlock[] = [];
  // Settled ask() holds replay as user-side text AFTER the tool_results (a
  // tool_use's result must lead the next user message) — see module header.
  const asks: LlmContentBlock[] = [];
  const resolved = new Set<string>();
  const requested = new Set<string>();
  for (const p of m.parts) {
    if (p.type === "text") {
      assistant.push({ type: "text", text: p.text });
    } else if (p.type === "reasoning") {
      // dropped on replay (see module header)
    } else if (p.type === "prose") {
      // dropped on replay — the prose text already replays verbatim inside its
      // program's tool_call input, so echoing it would double-bill the answer
    } else if (p.type === "ask") {
      const outcome = p.status === "answered"
        ? `the user answered: ${p.answer}`
        : p.status === "declined"
        ? "the user declined to answer"
        : "the turn was interrupted before an answer";
      asks.push({ type: "text", text: `[ask] ${p.question}\n→ ${outcome}` });
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
  if (results.length || asks.length) out.push({ role: "user", content: [...results, ...asks] });
  return out;
}

function stringifyOutput(output: unknown): string {
  return typeof output === "string" ? output : JSON.stringify(output);
}
