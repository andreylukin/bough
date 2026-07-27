/**
 * Stored parts → provider messages. The one place a persisted transcript becomes
 * something an `LlmClient` can be handed.
 *
 * WHY IT IS A SEPARATE MODULE. The mapping is pure and total, and it is where two
 * invariants live that nothing else in the system can enforce:
 *
 *   **1. Reasoning is dropped on replay (plan §6.4).** A `reasoning` part is
 *   persisted so the UI can fold it open. It is never sent back. There are no
 *   signed thinking blocks to echo across a turn boundary — the signature that made
 *   the block valid rode in `LlmBlock.meta`, which `blocksToParts` deliberately did
 *   not persist (llm/stream.ts) — so re-sending the text would be an unsigned
 *   imitation of thinking the model never gets credit for, at full input price. The
 *   *in-turn* echo is a different thing entirely and lives in `runner.ts`: within one
 *   turn the block travels back verbatim, meta and all, because some providers reject
 *   a tool call whose thinking was altered. Across turns, nothing.
 *
 *   **2. `ask` parts replay as plain text and can never re-block (plan §6.5).** A
 *   settled hold is a fact about what the user said, not a live question. Replaying
 *   it as anything the harness could re-raise would park a rebuilt thread on a
 *   question the user answered days ago, with no UI attached to answer it again. It
 *   becomes `[ask] <question> → the user answered: <answer>` in the user-side
 *   message, after the tool results — a tool_use's result must lead the user message
 *   that follows it, and text jammed in front of it is a provider 400.
 *
 * Two smaller rules that are equally not rediscoverable:
 *
 *   - **A `tool_use` with no matching `tool_result` gets a synthetic one.** A crash,
 *     an orphaned turn, or an interrupt between the call and its result leaves the
 *     pair open, and every provider rejects a thread in that state. The synthetic
 *     result says `(interrupted)` rather than pretending the tool succeeded.
 *   - **A lost attachment replays as placeholder text, never as a failure** (plan
 *     T2.2). The bytes live outside the parts JSON precisely so a row survives the
 *     file moving; the replay has to survive it too, or one deleted screenshot makes
 *     an entire session unreplayable.
 *
 * Purity: the image loader is injected. `messageToLlm` reads nothing and calls no
 * clock, so the whole mapping is testable with no filesystem and no `~/.bough`.
 */
import { isAbsolute, resolve } from "node:path";
import { attachmentsDir } from "../paths.ts";
import type { ImagePart, Message, Part } from "../schema/parts.ts";
import type { LlmContentBlock, LlmMessage } from "../types.ts";

// ---------------------------------------------------------------------------
// Attachments
// ---------------------------------------------------------------------------

/**
 * Loads an image part's bytes for replay. Returns `null` when the attachment is
 * gone — the caller degrades to placeholder text rather than failing the turn.
 *
 * Injected so replay is pure: the tests pass a map, production passes the reader
 * below.
 */
export type ImageLoader = (part: ImagePart) => { data: string; mediaType: string } | null;

/**
 * Resolve an image part's stored path. Relative paths resolve under
 * `~/.bough/attachments`, which is where every attachment is written; an absolute
 * path is taken as written, because it is one this server stored in its own
 * database, not a name that arrived in a request. (Contrast `confine`, which
 * guards path construction from *request* input — a different job.)
 */
export function attachmentPath(part: ImagePart): string {
  return isAbsolute(part.path) ? part.path : resolve(attachmentsDir(), part.path);
}

/**
 * The production loader: read the file, base64 it. Every failure mode — missing,
 * unreadable, no permission — is the same answer, `null`, because the replay's
 * response to all of them is identical and a distinction the caller cannot act on
 * is noise.
 */
export const readAttachment: ImageLoader = (part) => {
  try {
    return {
      data: Deno.readFileSync(attachmentPath(part)).toBase64(),
      mediaType: part.mediaType,
    };
  } catch {
    return null;
  }
};

/**
 * What the model sees in place of an image whose bytes are gone.
 *
 * It names the file the user named, and says plainly that the bytes are the missing
 * thing — not that the user sent nothing. A model told "[image]" with no
 * qualification will describe a picture it cannot see; told this, it asks for it
 * again or works without it (spec §6, error text is a product surface).
 */
export function lostAttachmentText(part: ImagePart): string {
  return `[image: ${part.name} — the attachment is no longer on disk, so it cannot be shown ` +
    `this time. It was ${part.size} bytes. Ask for it again if you need to see it.]`;
}

// ---------------------------------------------------------------------------
// One message
// ---------------------------------------------------------------------------

export interface ReplayOptions {
  /** Defaults to `readAttachment`. */
  loadImage?: ImageLoader;
}

/** Tool output is persisted as `unknown`; the wire wants a string. */
export function stringifyOutput(output: unknown): string {
  if (typeof output === "string") return output;
  if (output === undefined) return "";
  try {
    return JSON.stringify(output) ?? String(output);
  } catch {
    return String(output);
  }
}

/** How a settled hold reads to the model. Past tense, always — see invariant 2. */
function askText(p: Extract<Part, { type: "ask" }>): string {
  const outcome = p.status === "answered"
    ? `the user answered: ${p.answer ?? ""}`
    : p.status === "declined"
    ? "the user declined to answer"
    : "the turn was interrupted before an answer";
  return `[ask] ${p.question}\n→ ${outcome}`;
}

/**
 * One stored message → zero, one or two provider messages.
 *
 * - `user` and `system` → **one user message** of text and image blocks. System
 *   notes (a detached subagent's report, a background job's exit, artifact
 *   comments) are input *to* the model, never words it said, so they replay
 *   user-side (spec §4).
 * - `supervisor` → an **assistant message** (text + tool_use), then, when the round
 *   produced results or settled a hold, a **user message** of tool_result blocks
 *   followed by ask text.
 *
 * Empty in, empty out: a message that maps to no blocks yields no message at all
 * rather than an empty one, which providers reject.
 */
export function messageToLlm(m: Message, opts: ReplayOptions = {}): LlmMessage[] {
  const loadImage = opts.loadImage ?? readAttachment;

  if (m.role === "user" || m.role === "system") {
    const content: LlmContentBlock[] = [];
    for (const p of m.parts) {
      if (p.type === "text") {
        if (p.text) content.push({ type: "text", text: p.text });
      } else if (p.type === "image") {
        const loaded = loadImage(p);
        content.push(
          loaded
            ? { type: "image", data: loaded.data, mediaType: loaded.mediaType, name: p.name }
            : { type: "text", text: lostAttachmentText(p) },
        );
      }
      // Every other part kind is supervisor-side and cannot appear here.
    }
    return content.length ? [{ role: "user", content }] : [];
  }

  const assistant: LlmContentBlock[] = [];
  const results: LlmContentBlock[] = [];
  const asks: LlmContentBlock[] = [];
  const requested: string[] = [];
  const resolved = new Set<string>();

  for (const p of m.parts) {
    switch (p.type) {
      case "text":
        if (p.text) assistant.push({ type: "text", text: p.text });
        break;
      case "reasoning":
        // Invariant 1. Persisted for display only; nothing is emitted here.
        break;
      case "tool_call":
        requested.push(p.id);
        assistant.push({ type: "tool_use", id: p.id, name: p.name, input: p.input });
        break;
      case "tool_result":
        resolved.add(p.callId);
        results.push({
          type: "tool_result",
          toolUseId: p.callId,
          content: stringifyOutput(p.output),
          isError: p.isError,
        });
        break;
      case "ask":
        asks.push({ type: "text", text: askText(p) });
        break;
      case "image":
        // A picture the supervisor produced reaches the model as a system note
        // carrying the part (spec §6, `image()`), never inline on its own message.
        break;
    }
  }

  // Close every open pair, in call order, so the thread is one a provider accepts.
  for (const id of requested) {
    if (!resolved.has(id)) {
      results.push({
        type: "tool_result",
        toolUseId: id,
        content: "(interrupted — this call never returned a result)",
        isError: true,
      });
    }
  }

  const out: LlmMessage[] = [];
  if (assistant.length) out.push({ role: "assistant", content: assistant });
  // Results lead; ask text follows. Reversing this is a provider 400.
  if (results.length || asks.length) {
    out.push({ role: "user", content: [...results, ...asks] });
  }
  return out;
}

// ---------------------------------------------------------------------------
// A whole thread
// ---------------------------------------------------------------------------

export interface ThreadOptions extends ReplayOptions {
  /**
   * A message id to leave out — the pending supervisor message the turn is
   * currently producing. Replaying the thing you are about to write would show the
   * model an empty assistant turn at the end of its own history.
   */
  exclude?: string;
}

/**
 * Root→leaf thread → provider messages.
 *
 * Takes the already-ordered message list rather than a `Db`, because ordering is
 * the database's contract (`threadFor`: ancestors root→parent, then own, each by
 * `(created_at, rowid)`) and re-deriving it here would put two answers in the tree.
 */
export function buildThread(messages: readonly Message[], opts: ThreadOptions = {}): LlmMessage[] {
  const out: LlmMessage[] = [];
  for (const m of messages) {
    if (opts.exclude !== undefined && m.id === opts.exclude) continue;
    out.push(...messageToLlm(m, opts));
  }
  return out;
}

/**
 * Drop every reasoning block from an in-flight exchange.
 *
 * The in-turn echo (runner.ts) is only valid while the model that produced the
 * thinking is still the one being asked. It is not valid across a model swap, and
 * it is not valid once a provider has rejected the round — a stale or unverifiable
 * signature is a hard 400, and the round's text and tool calls are worth keeping
 * even when its thinking is not. An assistant message left with nothing but
 * reasoning disappears with it, because a content-less message is itself a 400.
 */
export function stripReasoning(messages: LlmMessage[]): void {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i];
    if (m.role !== "assistant") continue;
    m.content = m.content.filter((b) => b.type !== "reasoning");
    if (m.content.length === 0) messages.splice(i, 1);
  }
}
