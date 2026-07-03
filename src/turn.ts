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
import { maybeAutoTitle, type Titler } from "./supervisor/title.ts";
import { activeFor } from "./supervisor/skills.ts";
import { prepareWorkspace } from "./supervisor/workspace.ts";
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
 * ids ("claude-…") route to the Anthropic client; a provider-prefixed id
 * ("anthropic/…", "openai/…") routes to OpenRouter (see llm.clientFor).
 */
let currentModel = Deno.env.get("BOUGH_MODEL") ?? "claude-opus-4-8";

export function activeModel(): string {
  return currentModel;
}

export function setActiveModel(model: string): void {
  currentModel = model;
}

/**
 * Models offered in the picker. Anthropic ids go direct; the `openrouter/…` ids
 * route through OpenRouter (need OPENROUTER_API_KEY). Not exhaustive — the composer
 * accepts any id, this is just the quick-switch menu.
 */
export const MODELS: { id: string; label: string; provider: "anthropic" | "openrouter" }[] = [
  { id: "claude-opus-4-8", label: "Opus 4.8", provider: "anthropic" },
  { id: "claude-fable-5", label: "Fable 5", provider: "anthropic" },
  { id: "claude-sonnet-5", label: "Sonnet 5", provider: "anthropic" },
  { id: "claude-haiku-4-5", label: "Haiku 4.5", provider: "anthropic" },
  { id: "openai/gpt-5", label: "GPT-5 (OpenRouter)", provider: "openrouter" },
  { id: "google/gemini-2.5-pro", label: "Gemini 2.5 Pro (OpenRouter)", provider: "openrouter" },
];

const MAX_TOKENS = 64_000;
// Code-mode (SPEC §5): the supervisor plans and writes; the harness is the only
// executor. One program per round, CHECK-gated completion.
const SYSTEM = [
  "You are bough, a coding agent. You act ONLY through the run_steps tool: each call",
  "carries one JavaScript program that a deterministic harness executes in a sealed V8",
  "sandbox — you never touch the machine directly.",
  "Inside the program the entire capability surface is four async host functions:",
  "await bash(cmd) — shell in the sandboxed workspace, returns combined output;",
  "await read(path); await write(path, content); await edit(path, oldText, newText).",
  "console.log(...) is how you see anything — print what the next round needs.",
  "Write one program per round covering inspect → change → verify; prefer one",
  "substantial program over many tiny rounds.",
  "Commit a `check` early: a shell command that exits 0 iff the task's literal",
  "acceptance criteria hold. Set `done: true` when the work is complete — the harness",
  "re-runs the committed check and accepts done only if it passes.",
  "For pure questions or conversation, answer in plain text without calling run_steps.",
].join(" ");

/** Read the workspace AGENTS.md (capped) as a system-prompt section, or null if absent. */
async function readAgentsFile(cwd: string): Promise<string | null> {
  try {
    const text = await Deno.readTextFile(join(cwd, "AGENTS.md"));
    if (!text.trim()) return null;
    // Cap so a huge file can't crowd out the task; the model can read the rest itself.
    const body = text.length > 12_000 ? text.slice(0, 12_000) + "\n…(truncated)" : text;
    return "\n\n# Project rules (AGENTS.md)\nThe workspace root has an AGENTS.md — treat it " +
      "as authoritative for build/test commands, conventions, and what \"done\" means:\n\n" +
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
export function beginTurn(ctx: TurnCtx, sessionId: string): { message: Message; done: Promise<void> } {
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
    // sees every queued user message since the last supervisor reply.
    if (queued.delete(sessionId)) beginTurn(ctx, sessionId);
  });
  return { message, done };
}

/** Sessions with a user message queued while a turn was in flight (see startUserTurn). */
const queued = new Set<string>();

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
  // One turn per session: if one is already running, the message is persisted and shown
  // now, and a single follow-up turn drains the queue when the current one finishes.
  if (isTurnRunning(sessionId)) {
    queued.add(sessionId);
    return { userMessage, done: Promise.resolve() };
  }
  const { done } = beginTurn(ctx, sessionId);
  return { userMessage, done };
}

async function drive(ctx: TurnCtx, message: Message, signal?: AbortSignal): Promise<void> {
  const { db, bus } = ctx;
  const sessionId = message.sessionId;
  const messageId = message.id;
  const model = ctx.model ?? activeModel();
  const llm = ctx.llm ?? clientFor(model);
  const tools = ctx.tools ?? defaultTools;
  // Newest round's input_tokens ≈ the live context size; output accumulates.
  let contextTokens = 0;
  let outputTokens = 0;

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
    const toolCtx: ToolRunCtx = {
      workspace: prepared.cwd,
      sandbox: prepared.sandboxed ? { sessionDir: prepared.sessionDir } : undefined,
      // Per-turn harness state: run_steps commits the CHECK here (SPEC §5 gating).
      turn: {},
    };

    const messages = buildHistory(db, sessionId, messageId);
    // Inline any @path references in the triggering message so the model sees the
    // file content, not just the name. Only for workspace sessions (files exist).
    if (prepared.sandboxed) inlineFileReferences(messages, prepared.cwd);
    // Project rules: an AGENTS.md at the workspace root is authoritative for build/test
    // commands, conventions, and what "done" means — inject it into the system prompt.
    const agents = prepared.sandboxed ? await readAgentsFile(prepared.cwd) : null;
    // Skills: `/name` in the triggering user message pulls that skill's
    // instructions into the system prompt for this run (supervisor/skills.ts).
    const system = SYSTEM + (agents ?? "") + activeFor(lastUserText(db, sessionId));
    const toolDefs = tools.map((t) => ({
      name: t.name,
      description: t.description,
      inputSchema: jsonSchema(t),
    }));

    // Unbounded on purpose: the loop ends when the model stops asking for tools or
    // the CHECK gate accepts done; interruptTurn is the user's brake on a runaway.
    for (let round = 0;; round++) {
      if (signal?.aborted) throw new InterruptedError();
      const result = await llm.run(
        { model, system, maxTokens: MAX_TOKENS, messages, tools: toolDefs },
        (delta) => bus.publish({ type: "message.delta", sessionId, data: { messageId, delta } }),
        signal,
      );

      if (result.usage) {
        contextTokens = result.usage.inputTokens; // last round = current context size
        outputTokens += result.usage.outputTokens;
      }

      const assistant: LlmContentBlock[] = [];
      for (const block of result.content) {
        if (block.type === "text") {
          append({ type: "text", text: block.text });
          assistant.push({ type: "text", text: block.text });
        } else if (block.type === "reasoning") {
          // Persisted for display; dropped from the replayed assistant turn.
          append({ type: "reasoning", text: block.text });
        } else if (block.type === "tool_use") {
          append({ type: "tool_call", id: block.id, name: block.name, input: block.input });
          assistant.push(block);
        }
      }
      messages.push({ role: "assistant", content: assistant });
      checkpoint(db, turn.id, `round:${round + 1}`);

      const toolUses = result.content.filter((b) => b.type === "tool_use");
      if (result.stopReason !== "tool_use" || toolUses.length === 0) break;

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
      if (doneAccepted) break;
    }

    db.updateMessage(messageId, parts, false);
    finishTurn(db, turn.id, "done");
    bus.publish({ type: "message.finished", sessionId, data: { messageId } });
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
    finishTurn(db, turn.id, interrupted ? "interrupted" : "error");
    bus.publish({ type: "message.part", sessionId, data: { messageId, part: note } });
    bus.publish({ type: "message.finished", sessionId, data: { messageId } });
  } finally {
    // Persist token usage for the context meter and announce it (usage.updated).
    // contextTokens stays 0 for a stream that errored before its first usage report.
    if (contextTokens > 0 || outputTokens > 0) {
      const prev = db.sessionUsage(sessionId);
      db.setSessionUsage(sessionId, contextTokens, prev.outputTokens + outputTokens);
      bus.publish({
        type: "usage.updated",
        sessionId,
        data: { sessionId, contextTokens, outputTokens: prev.outputTokens + outputTokens },
      });
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
  if (m.role === "user") {
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
