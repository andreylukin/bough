/**
 * The LLM boundary. turn.ts speaks only in the normalized shapes below; the real
 * Anthropic SDK lives behind `anthropicClient` and nowhere else. Tests inject a
 * scripted fake implementing `LlmClient`, so the runner never touches the network.
 *
 * Normalization choices:
 *   - Content blocks are text / tool_use / reasoning / tool_result. We collapse the
 *     SDK's block zoo to these; anything else (server tools) is dropped, which is
 *     safe because we don't enable those features.
 *   - We do not REQUEST extended thinking (the `thinking` param is omitted), but
 *     Claude 5 models think adaptively by default and it cannot be turned off, so
 *     thinking / redacted_thinking blocks do arrive. They ride the `reasoning`
 *     block's `meta` as the raw SDK block and are replayed VERBATIM within a turn —
 *     the API requires a tool round's signed thinking to precede its tool_use on
 *     the next round. Cross-turn replay still drops reasoning (history mapper).
 *
 * Hardening: clientFor wraps every provider in withRetries — transient failures
 * (429/5xx, network faults, dropped/truncated/stalled streams) back off and retry;
 * the fetch clients treat a stream that ends without its completion marker as an
 * error rather than returning a partial round (half-assembled tool calls).
 */
import Anthropic from "@anthropic-ai/sdk";

export type LlmBlock =
  | { type: "text"; text: string }
  // `meta` is an opaque provider payload replayed verbatim within a turn — the
  // OpenAI Responses API requires a function_call's reasoning item (encrypted
  // content included) to precede it in the next round's input.
  | { type: "reasoning"; text: string; meta?: unknown }
  | { type: "tool_use"; id: string; name: string; input: unknown };

/** A block as it appears in a request message (adds tool_result/image to LlmBlock). */
export type LlmContentBlock =
  | LlmBlock
  | { type: "tool_result"; toolUseId: string; content: string; isError: boolean }
  // A user-attached image, base64-encoded at history-assembly time (turn.ts).
  // Every provider maps it to its native image-input shape; `name` labels it in
  // errors/placeholders.
  | { type: "image"; data: string; mediaType: string; name: string };

export interface LlmMessage {
  role: "user" | "assistant";
  content: LlmContentBlock[];
}

export interface LlmToolDef {
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
}

/** Thinking depth (the API's `output_config.effort` vocabulary). */
export type Effort = "low" | "medium" | "high" | "xhigh" | "max";
export const EFFORTS: Effort[] = ["low", "medium", "high", "xhigh", "max"];

export interface LlmParams {
  model: string;
  /**
   * The STABLE system prefix. Prompt-cache contract: this must be byte-identical
   * across sessions and turns (per delegation tier — see turn.ts's assembly), so
   * the provider cache can share it machine-wide. Anything carrying per-session
   * facts (paths, session ids, catalogs) belongs in `systemVolatile`, never here
   * — one volatile byte early in the prefix defeats cross-session sharing.
   */
  system?: string;
  /**
   * Per-session/per-turn system suffix (workspace + scratchpad paths, AGENTS.md,
   * MCP catalog, skills, running-subagent notes). Sent AFTER `system` with its
   * own cache breakpoint, so it still caches across turns within a session
   * without poisoning the shared stable prefix. Providers without breakpoints
   * see the two tiers joined (stable first — implicit prefix caching still wins).
   */
  systemVolatile?: string;
  maxTokens: number;
  messages: LlmMessage[];
  tools: LlmToolDef[];
  /**
   * "none" forbids tool calls for this round, forcing a plain-text reply. The
   * turn runner's last resort against a mute turn end: Claude 5's adaptive
   * thinking sometimes answers a report request with an empty thinking block +
   * stop; with tools off it reliably writes the text instead.
   */
  toolChoice?: "none";
  /**
   * Thinking depth. When set, the Anthropic client requests adaptive thinking
   * with summarized display (so the reasoning is visible) at this effort;
   * OpenAI maps it onto `reasoning.effort` (capped at "high"); OpenRouter
   * ignores it (support varies per routed model). Unset = provider defaults,
   * request shape unchanged. Not supported by every model (e.g. Haiku 4.5
   * rejects it) — the resulting API error surfaces as a turn error.
   */
  effort?: Effort;
}

/** Token usage for one round; summed across a turn for the context meter. */
export interface LlmUsage {
  inputTokens: number;
  outputTokens: number;
  /** Prompt tokens served from the provider's prompt cache this round. */
  cacheReadTokens?: number;
  /** Prompt tokens newly written to the cache this round. */
  cacheCreationTokens?: number;
}

export interface LlmResult {
  content: LlmBlock[];
  stopReason: string;
  usage?: LlmUsage;
}

export interface LlmClient {
  /**
   * Run one round. `onText` receives streamed text deltas as they arrive.
   * `signal`, when aborted, cancels the in-flight request (the caller catches
   * the resulting abort error and treats the turn as interrupted).
   */
  run(params: LlmParams, onText: (delta: string) => void, signal?: AbortSignal): Promise<LlmResult>;
}

// ---- transient-failure retries ----------------------------------------------

/**
 * A provider/transport failure. `status` (when known) drives retry classification;
 * no status = a transport fault (dropped, truncated, or stalled stream), always
 * retryable. `retryAfterMs` carries the provider's Retry-After hint.
 */
export class LlmError extends Error {
  constructor(message: string, readonly status?: number, readonly retryAfterMs?: number) {
    super(message);
    this.name = "LlmError";
  }
}

const retryableStatus = (s: number): boolean => s === 408 || s === 429 || s >= 500;

/** Should this failure be re-attempted? User aborts and caller mistakes (4xx) never are. */
export function isRetryable(err: unknown): boolean {
  const e = err as { name?: string; status?: unknown } | null;
  if (e?.name === "AbortError" || e?.name === "APIUserAbortError") return false;
  if (err instanceof LlmError) return err.status === undefined || retryableStatus(err.status);
  // Anthropic SDK: APIError carries `.status`; connection failures carry none.
  if (typeof e?.status === "number") return retryableStatus(e.status);
  if (e?.name === "APIConnectionError" || e?.name === "APIConnectionTimeoutError") return true;
  return err instanceof TypeError; // fetch network failure
}

export interface RetryOpts {
  /** Observes each re-attempt: called after a retryable failure, before the backoff sleep. */
  onRetry?: (info: { attempt: number; maxAttempts: number; error: Error; delayMs: number }) => void;
  maxAttempts?: number;
  baseDelayMs?: number;
}

// 6 attempts ≈ 15–31s of jittered backoff (1+2+4+8+16s halved-to-full): long
// enough to ride out a network-path flap (e.g. a TLS BadRecordMac streak on a
// bad IPv6 route), short enough that a truly dead network fails the turn in
// under a minute. The backoff sleep is abort-aware, so Esc still cuts it short.
const MAX_ATTEMPTS = 6;
const BASE_DELAY_MS = 1000;

/**
 * Transparent retries around an LlmClient. Sound because a round has no side
 * effects until run() resolves (the turn loop executes tools afterwards), so
 * re-sending identical params can at worst repeat streamed text deltas — the
 * caller's onRetry hook is where the UI resets its streaming buffer.
 */
export function withRetries(inner: LlmClient, opts: RetryOpts = {}): LlmClient {
  const maxAttempts = opts.maxAttempts ?? MAX_ATTEMPTS;
  const baseDelayMs = opts.baseDelayMs ?? BASE_DELAY_MS;
  return {
    async run(params, onText, signal) {
      for (let attempt = 1;; attempt++) {
        try {
          return await inner.run(params, onText, signal);
        } catch (err) {
          if (attempt >= maxAttempts || signal?.aborted || !isRetryable(err)) throw err;
          // Exponential backoff with jitter; the provider's Retry-After wins when longer.
          const backoff = baseDelayMs * 2 ** (attempt - 1) * (0.5 + Math.random() / 2);
          const delayMs = Math.round(Math.max(retryAfterHint(err) ?? 0, backoff));
          opts.onRetry?.({ attempt, maxAttempts, error: err as Error, delayMs });
          await delay(delayMs, signal);
        }
      }
    },
  };
}

/** The provider's Retry-After, in ms, when the error carries one. */
function retryAfterHint(err: unknown): number | undefined {
  if (err instanceof LlmError) return err.retryAfterMs;
  // Anthropic SDK errors expose the response headers.
  const headers = (err as { headers?: { get?: (k: string) => string | null } }).headers;
  const secs = Number(headers?.get?.("retry-after"));
  return Number.isFinite(secs) && secs > 0 ? secs * 1000 : undefined;
}

function delay(ms: number, signal?: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(new DOMException("interrupted during retry backoff", "AbortError"));
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });
}

/** A stream that stops sending bytes for this long is treated as dropped. */
const STALL_TIMEOUT_MS = 60_000;

/** reader.read() with a stall guard — a silently dead connection surfaces as a
 * retryable LlmError instead of hanging the turn until the user interrupts. */
function readWithStallGuard(
  reader: ReadableStreamDefaultReader<Uint8Array>,
  provider: string,
): Promise<ReadableStreamReadResult<Uint8Array>> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      reader.cancel().catch(() => {});
      reject(new LlmError(`${provider}: stream stalled (no data for ${STALL_TIMEOUT_MS / 1000}s)`));
    }, STALL_TIMEOUT_MS);
    reader.read().then(resolve, reject).finally(() => clearTimeout(timer));
  });
}

/** Map a non-2xx provider response to a classified LlmError (never returns). */
async function throwHttpError(provider: string, res: Response): Promise<never> {
  const retryAfter = Number(res.headers.get("retry-after"));
  throw new LlmError(
    `${provider}: ${res.status} ${await res.text().catch(() => "")}`,
    res.status,
    Number.isFinite(retryAfter) && retryAfter > 0 ? retryAfter * 1000 : undefined,
  );
}

// ---- system-prompt tiers ---------------------------------------------------

/**
 * The two system tiers as Anthropic system blocks, a 1-HOUR cache breakpoint on
 * each (stable first — the API caches everything before a breakpoint, so the
 * volatile block must never precede the stable one). Undefined when there is no
 * system text at all. Exported for tests: breakpoint placement IS the cache
 * economics.
 */
export function anthropicSystemBlocks(
  p: Pick<LlmParams, "system" | "systemVolatile">,
): Anthropic.TextBlockParam[] | undefined {
  const blocks = [p.system, p.systemVolatile]
    .filter((t): t is string => !!t)
    .map((text) => ({
      type: "text" as const,
      text,
      // `ttl` postdates the pinned SDK's types but serializes fine (like
      // effortParams' spread) — hence the cast.
      cache_control: { type: "ephemeral", ttl: "1h" } as Anthropic.CacheControlEphemeral,
    }));
  return blocks.length > 0 ? blocks : undefined;
}

/**
 * Both tiers joined into one string (stable first) for providers that take a
 * single system/instructions field and cache prefixes implicitly (OpenAI,
 * OpenRouter). Undefined when both are empty.
 */
export function joinedSystem(
  p: Pick<LlmParams, "system" | "systemVolatile">,
): string | undefined {
  const s = (p.system ?? "") + (p.systemVolatile ?? "");
  return s || undefined;
}

// ---- real client -----------------------------------------------------------

/** Our normalized message → the Anthropic wire shape. Exported for tests. */
export function toApiMessage(m: LlmMessage): Anthropic.MessageParam {
  const content = m.content.flatMap((b): Anthropic.ContentBlockParam[] => {
    switch (b.type) {
      case "text":
        return [{ type: "text", text: b.text }];
      case "image":
        return [{
          type: "image",
          source: {
            type: "base64",
            media_type: b.mediaType as Anthropic.Base64ImageSource["media_type"],
            data: b.data,
          },
        }];
      case "reasoning": {
        // An Anthropic thinking block replays verbatim (signature included) — the
        // API rejects a tool_use whose preceding thinking was altered or dropped.
        const meta = b.meta as { type?: string } | undefined;
        if (meta?.type === "thinking" || meta?.type === "redacted_thinking") {
          return [meta as Anthropic.ContentBlockParam];
        }
        // Foreign/historical reasoning (an OpenAI Responses concern) degrades to
        // prose; summary-less items would be empty text blocks, which the API rejects.
        return b.text.trim() ? [{ type: "text", text: b.text }] : [];
      }
      case "tool_use":
        return [{ type: "tool_use", id: b.id, name: b.name, input: b.input ?? {} }];
      case "tool_result":
        return [{
          type: "tool_result",
          tool_use_id: b.toolUseId,
          content: b.content,
          is_error: b.isError,
        }];
    }
  });
  return { role: m.role, content };
}

function fromApiBlock(block: Anthropic.ContentBlock): LlmBlock | undefined {
  switch (block.type) {
    case "text":
      return { type: "text", text: block.text };
    case "thinking":
      // Keep the raw block (signature included) for verbatim in-turn replay.
      return { type: "reasoning", text: block.thinking, meta: block };
    case "redacted_thinking":
      // Nothing displayable, but the block must still be echoed on the next round.
      return { type: "reasoning", text: "", meta: block };
    case "tool_use":
      return { type: "tool_use", id: block.id, name: block.name, input: block.input };
    default:
      return undefined; // server tools, etc. — not used here
  }
}

// Thinking depth: adaptive thinking (explicit — Opus 4.8 runs WITHOUT thinking
// when the param is omitted) with summarized display so the UI's reasoning folds
// carry text, at the chosen effort. Returned as a plain object and spread in
// (spreads skip excess-property checks) — the pinned SDK (0.68) predates these
// fields in its types but serializes unknown request keys fine.
function effortParams(effort?: Effort, model?: string): Record<string, unknown> {
  // Adaptive thinking exists on the Claude 5 family and Opus 4.8+ only — sending
  // it to e.g. Haiku 4.5 is a hard 400, so a set effort must not kill the turn.
  const supported = model === undefined ||
    /claude-(fable|mythos|sonnet)-5|opus-4-[89]/.test(model);
  return effort && supported
    ? {
      thinking: { type: "adaptive", display: "summarized" },
      output_config: { effort },
    }
    : {};
}

export function anthropicClient(): LlmClient {
  // maxRetries 0: the retry policy lives in withRetries (see clientFor), uniform
  // across providers — the SDK's own pre-stream retries would stack under it.
  const client = new Anthropic({ maxRetries: 0 });
  return {
    async run(params, onText, signal) {
      // Prompt caching, three breakpoints (longer-TTL breakpoints must precede
      // shorter; budget is 4):
      //   - breakpoint on the STABLE system block caches tools + the stable
      //     system prefix at a 1-HOUR TTL. That prefix is byte-identical across
      //     sessions (turn.ts's assembly contract), so it warms new sessions and
      //     survives a lunch break (writes bill 2x vs 1.25x — break-even is
      //     roughly one extra hit);
      //   - breakpoint on the VOLATILE system block (per-session paths, AGENTS.md,
      //     MCP/skills) at 1h too: caches across turns within a session without
      //     splintering the shared prefix;
      //   - breakpoint on the final block of the final message extends the cached
      //     conversation prefix each round at the default 5-minute sliding TTL
      //     (the API reuses the longest previously cached prefix).
      // Verifying in the field (~/.bough/bough.db): the sessions table accumulates
      // cache_read_total / cache_write_total alongside input_tokens —
      //   SELECT date(created_at/1000,'unixepoch') d,
      //          1.0*sum(cache_read_total)/sum(input_tokens) cache_share
      //   FROM sessions GROUP BY d ORDER BY d;
      // cache_share is the cache-read share of billed input; pre-split field data
      // sat at ~0.34 (Claude Code on the same machine: ~0.999) — it should climb
      // toward that after this split.
      const messages = params.messages.map(toApiMessage);
      const lastContent = messages.at(-1)?.content;
      if (Array.isArray(lastContent) && lastContent.length > 0) {
        (lastContent[lastContent.length - 1] as { cache_control?: unknown }).cache_control = {
          type: "ephemeral",
        };
      }
      const stream = client.messages.stream({
        model: params.model,
        max_tokens: params.maxTokens,
        system: anthropicSystemBlocks(params),
        messages,
        tools: params.tools.map((t) => ({
          name: t.name,
          description: t.description,
          input_schema: t.inputSchema as Anthropic.Tool.InputSchema,
        })),
        ...(params.toolChoice ? { tool_choice: { type: params.toolChoice } } : {}),
        ...effortParams(params.effort, params.model),
      }, { signal });
      stream.on("text", (delta) => onText(delta));
      const final = await stream.finalMessage();
      const content = final.content
        .map(fromApiBlock)
        .filter((b): b is LlmBlock => b !== undefined);
      const usage: LlmUsage = {
        // input_tokens is the uncached remainder; add cache reads/writes for the
        // true prompt size the context meter should show.
        inputTokens: final.usage.input_tokens +
          (final.usage.cache_read_input_tokens ?? 0) +
          (final.usage.cache_creation_input_tokens ?? 0),
        outputTokens: final.usage.output_tokens,
        cacheReadTokens: final.usage.cache_read_input_tokens ?? 0,
        cacheCreationTokens: final.usage.cache_creation_input_tokens ?? 0,
      };
      return { content, stopReason: final.stop_reason ?? "end_turn", usage };
    },
  };
}

// ---- OpenAI-compatible providers (OpenRouter + OpenAI) --------------------
//
// A separate provider family, not the Anthropic client with a base_url override: it
// speaks the OpenAI chat-completions shape (different message/tool encoding), so it
// gets its own hand-rolled fetch client, shared by OpenRouter and OpenAI proper.
// OpenRouter is selected by a "vendor/model" id (e.g. "openai/gpt-4o"); OpenAI proper
// by an "openai:model" id (see clientFor). Need OPENROUTER_API_KEY / OPENAI_API_KEY.

interface OpenAIToolCall {
  index: number;
  id?: string;
  function?: { name?: string; arguments?: string };
}

/** Flatten our multi-block messages into OpenAI chat messages (tool_results split
 * out). Exported for tests. */
export function toOpenAIMessages(system: string | undefined, messages: LlmMessage[]): unknown[] {
  const out: unknown[] = [];
  if (system) out.push({ role: "system", content: system });
  for (const m of messages) {
    if (m.role === "assistant") {
      const text = m.content.filter((b) => b.type === "text").map((b) =>
        (b as { text: string }).text
      ).join("");
      const toolCalls = m.content
        .filter((b) => b.type === "tool_use")
        .map((b) => {
          const t = b as { id: string; name: string; input: unknown };
          return {
            id: t.id,
            type: "function",
            function: { name: t.name, arguments: JSON.stringify(t.input ?? {}) },
          };
        });
      out.push({
        role: "assistant",
        content: text || null,
        ...(toolCalls.length ? { tool_calls: toolCalls } : {}),
      });
    } else {
      // user turn: text/image blocks → one user message; tool_result blocks →
      // one tool msg each. With no images the content stays a plain string
      // (wire shape unchanged); with images it becomes the multimodal parts
      // array — image_url data URLs, which OpenRouter passes through to
      // vision-capable models (a non-vision model errors, surfaced as-is).
      const texts = m.content.filter((b) => b.type === "text");
      const images = m.content.filter((b) => b.type === "image");
      const results = m.content.filter((b) => b.type === "tool_result");
      if (texts.length || images.length) {
        const joined = texts.map((b) => (b as { text: string }).text).join("\n");
        out.push({
          role: "user",
          content: images.length
            ? [
              ...(joined ? [{ type: "text", text: joined }] : []),
              ...images.map((b) => {
                const i = b as { data: string; mediaType: string };
                return {
                  type: "image_url",
                  image_url: { url: `data:${i.mediaType};base64,${i.data}` },
                };
              }),
            ]
            : joined,
        });
      }
      for (const r of results) {
        const t = r as { toolUseId: string; content: string };
        out.push({ role: "tool", tool_call_id: t.toolUseId, content: t.content });
      }
    }
  }
  return out;
}

export function openrouterClient(): LlmClient {
  return openAICompatClient({
    provider: "openrouter",
    url: "https://openrouter.ai/api/v1/chat/completions",
    apiKeyEnv: "OPENROUTER_API_KEY",
    extraHeaders: { "x-title": "bough" },
  });
}

// ---- OpenAI proper: the Responses API --------------------------------------
//
// Chat/completions can't combine function tools with reasoning on the gpt-5/o*
// families, so OpenAI rides /v1/responses instead. Stateless (store:false): each
// round replays the whole history as input items, with reasoning items (their
// encrypted content requested via `include`) echoed back verbatim before their
// function_call — the API rejects a function_call whose reasoning item is missing.
// Reasoning items ride LlmBlock.meta through turn.ts's in-memory round loop;
// across turns the history mapper drops them, and old function_calls replay
// bare (accepted — the pairing rule binds items of the live response chain).
// Picker ids carry an "openai:" prefix (openai:gpt-5); stripped for the wire.

/** LlmMessages → Responses `input` items. Exported for tests. */
export function toResponsesInput(messages: LlmMessage[]): unknown[] {
  const out: unknown[] = [];
  for (const m of messages) {
    for (const b of m.content) {
      if (b.type === "text") {
        out.push(
          m.role === "user"
            ? { role: "user", content: [{ type: "input_text", text: b.text }] }
            : { role: "assistant", content: [{ type: "output_text", text: b.text }] },
        );
      } else if (b.type === "image") {
        // Images only occur on user messages (turn.ts history assembly).
        out.push({
          role: "user",
          content: [{ type: "input_image", image_url: `data:${b.mediaType};base64,${b.data}` }],
        });
      } else if (b.type === "reasoning") {
        if (b.meta) out.push(b.meta); // the raw reasoning item, replayed verbatim
      } else if (b.type === "tool_use") {
        out.push({
          type: "function_call",
          call_id: b.id,
          name: b.name,
          arguments: JSON.stringify(b.input ?? {}),
        });
      } else if (b.type === "tool_result") {
        out.push({ type: "function_call_output", call_id: b.toolUseId, output: b.content });
      }
    }
  }
  return out;
}

interface ResponsesItem {
  type?: string;
  call_id?: string;
  name?: string;
  arguments?: string;
  content?: { type?: string; text?: string }[];
  summary?: { text?: string }[];
}

/** A Responses `output` array → our normalized blocks. Exported for tests. */
export function fromResponsesOutput(output: ResponsesItem[]): LlmBlock[] {
  const blocks: LlmBlock[] = [];
  for (const item of output) {
    if (item.type === "message") {
      const text = (item.content ?? [])
        .filter((c) => c.type === "output_text")
        .map((c) => c.text ?? "")
        .join("");
      if (text) blocks.push({ type: "text", text });
    } else if (item.type === "function_call") {
      let input: unknown = {};
      try {
        input = JSON.parse(item.arguments ?? "{}");
      } catch {
        // leave {} — the tool layer reports the schema violation
      }
      blocks.push({ type: "tool_use", id: item.call_id ?? "", name: item.name ?? "", input });
    } else if (item.type === "reasoning") {
      const text = (item.summary ?? []).map((s) => s.text ?? "").join("\n");
      blocks.push({ type: "reasoning", text, meta: item });
    }
  }
  return blocks;
}

export function openaiClient(): LlmClient {
  const base = Deno.env.get("OPENAI_API_BASE") ?? "https://api.openai.com";
  return {
    async run(params, onText, signal) {
      const apiKey = Deno.env.get("OPENAI_API_KEY");
      if (!apiKey) throw new Error("OPENAI_API_KEY is not set");
      const model = params.model.startsWith("openai:")
        ? params.model.slice("openai:".length)
        : params.model;
      const res = await fetch(`${base}/v1/responses`, {
        method: "POST",
        signal,
        headers: { authorization: `Bearer ${apiKey}`, "content-type": "application/json" },
        body: JSON.stringify({
          model,
          instructions: joinedSystem(params),
          max_output_tokens: params.maxTokens,
          stream: true,
          store: false,
          include: ["reasoning.encrypted_content"],
          // Thinking depth: the Responses API caps reasoning effort at "high".
          ...(params.effort
            ? {
              reasoning: {
                effort: params.effort === "xhigh" || params.effort === "max"
                  ? "high"
                  : params.effort,
              },
            }
            : {}),
          input: toResponsesInput(params.messages),
          tools: params.tools.map((t) => ({
            type: "function",
            name: t.name,
            description: t.description,
            parameters: t.inputSchema,
          })),
          ...(params.toolChoice ? { tool_choice: params.toolChoice } : {}),
        }),
      });
      if (!res.ok) await throwHttpError("openai", res);
      if (!res.body) throw new LlmError("openai: empty response body");

      // Deltas stream for the live feel; the final content comes whole from the
      // response.completed payload (no per-item assembly to get wrong).
      let final: {
        output?: ResponsesItem[];
        status?: string;
        incomplete_details?: { reason?: string };
        error?: { code?: string; message?: string };
        usage?: {
          input_tokens?: number;
          output_tokens?: number;
          input_tokens_details?: { cached_tokens?: number };
        };
      } | undefined;
      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await readWithStallGuard(reader, "openai");
        if (done) break;
        buffer += dec.decode(value, { stream: true });
        let nl: number;
        while ((nl = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, nl).trim();
          buffer = buffer.slice(nl + 1);
          if (!line.startsWith("data:")) continue;
          const data = line.slice(5).trim();
          if (data === "[DONE]") continue;
          let ev: { type?: string; delta?: string; response?: typeof final; message?: string };
          try {
            ev = JSON.parse(data);
          } catch {
            continue;
          }
          if (ev.type === "response.output_text.delta" && ev.delta) onText(ev.delta);
          else if (ev.type === "response.completed" && ev.response) final = ev.response;
          else if (
            (ev.type === "response.failed" || ev.type === "error") && !final
          ) {
            // Mid-stream failure events are server-side (the request itself was
            // accepted) — classify retryable, rate limits by their error code.
            const code = ev.response?.error?.code ?? "";
            throw new LlmError(
              `openai: ${JSON.stringify(ev)}`,
              code.includes("rate_limit") ? 429 : 500,
            );
          } else if (ev.type === "response.incomplete" && ev.response) final = ev.response;
        }
      }
      // No status → transport fault → retryable.
      if (!final) throw new LlmError("openai: stream ended without response.completed");

      const content = fromResponsesOutput(final.output ?? []);
      const stopReason = content.some((b) => b.type === "tool_use")
        ? "tool_use"
        : final.status === "incomplete" && final.incomplete_details?.reason === "max_output_tokens"
        ? "max_tokens"
        : "end_turn";
      const usage: LlmUsage = {
        inputTokens: final.usage?.input_tokens ?? 0,
        outputTokens: final.usage?.output_tokens ?? 0,
        cacheReadTokens: final.usage?.input_tokens_details?.cached_tokens ?? 0,
        cacheCreationTokens: 0,
      };
      return { content, stopReason, usage };
    },
  };
}

interface OpenAICompatOpts {
  /** Names the provider in errors ("openrouter: 401 …", "…_API_KEY is not set"). */
  provider: string;
  url: string;
  /** Read at run() time, so a key set at runtime applies without a restart. */
  apiKeyEnv: string;
  extraHeaders?: Record<string, string>;
  /** Map our model id to the provider's wire id. */
  mapModel?: (model: string) => string;
}

// The shared OpenAI chat-completions streaming client behind both OpenRouter and
// OpenAI proper — same wire shape; only the URL, key, and headers differ.
function openAICompatClient(opts: OpenAICompatOpts): LlmClient {
  return {
    async run(params, onText, signal) {
      const apiKey = Deno.env.get(opts.apiKeyEnv);
      if (!apiKey) throw new Error(`${opts.apiKeyEnv} is not set`);
      const res = await fetch(opts.url, {
        method: "POST",
        signal,
        headers: {
          authorization: `Bearer ${apiKey}`,
          "content-type": "application/json",
          ...opts.extraHeaders,
        },
        body: JSON.stringify({
          model: opts.mapModel ? opts.mapModel(params.model) : params.model,
          max_tokens: params.maxTokens,
          stream: true,
          stream_options: { include_usage: true },
          messages: toOpenAIMessages(joinedSystem(params), params.messages),
          tools: params.tools.map((t) => ({
            type: "function",
            function: { name: t.name, description: t.description, parameters: t.inputSchema },
          })),
          ...(params.toolChoice ? { tool_choice: params.toolChoice } : {}),
        }),
      });
      if (!res.ok) await throwHttpError(opts.provider, res);
      if (!res.body) throw new LlmError(`${opts.provider}: empty response body`);

      let text = "";
      const toolCalls = new Map<number, OpenAIToolCall>();
      let finishReason = "stop";
      let usage: LlmUsage | undefined;
      // Whether the stream reached a proper end ([DONE] or a finish_reason). A
      // stream that just closes was cut mid-response — returning the partial
      // round as success would run half-assembled tool calls.
      let ended = false;

      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await readWithStallGuard(reader, opts.provider);
        if (done) break;
        buffer += dec.decode(value, { stream: true });
        let nl: number;
        while ((nl = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, nl).trim();
          buffer = buffer.slice(nl + 1);
          if (!line.startsWith("data:")) continue;
          const data = line.slice(5).trim();
          if (data === "[DONE]") {
            ended = true;
            continue;
          }
          let chunk: {
            choices?: {
              delta?: { content?: string; tool_calls?: OpenAIToolCall[] };
              finish_reason?: string;
            }[];
            error?: { message?: string; code?: number | string };
            usage?: {
              prompt_tokens?: number;
              completion_tokens?: number;
              prompt_tokens_details?: { cached_tokens?: number };
            };
          };
          try {
            chunk = JSON.parse(data);
          } catch {
            continue;
          }
          // OpenRouter surfaces an upstream provider failure as a terminal `error`
          // chunk on an otherwise-200 stream; without this it would silently pass
          // the partial round off as success.
          if (chunk.error) {
            throw new LlmError(
              `${opts.provider}: ${chunk.error.message ?? JSON.stringify(chunk.error)}`,
              typeof chunk.error.code === "number" ? chunk.error.code : undefined,
            );
          }
          if (chunk.usage) {
            usage = {
              inputTokens: chunk.usage.prompt_tokens ?? 0,
              outputTokens: chunk.usage.completion_tokens ?? 0,
              // OpenRouter relays the upstream provider's cache hits (OpenAI-shape).
              cacheReadTokens: chunk.usage.prompt_tokens_details?.cached_tokens ?? 0,
            };
          }
          const choice = chunk.choices?.[0];
          if (!choice) continue;
          if (choice.finish_reason) {
            finishReason = choice.finish_reason;
            ended = true;
          }
          const delta = choice.delta;
          if (delta?.content) {
            text += delta.content;
            onText(delta.content);
          }
          for (const tc of delta?.tool_calls ?? []) {
            const cur = toolCalls.get(tc.index) ?? { index: tc.index };
            if (tc.id) cur.id = tc.id;
            if (tc.function?.name) cur.function = { ...cur.function, name: tc.function.name };
            if (tc.function?.arguments) {
              cur.function = {
                ...cur.function,
                arguments: (cur.function?.arguments ?? "") + tc.function.arguments,
              };
            }
            toolCalls.set(tc.index, cur);
          }
        }
      }
      // No status → transport fault → retryable.
      if (!ended) throw new LlmError(`${opts.provider}: stream truncated before completion`);

      const content: LlmBlock[] = [];
      if (text) content.push({ type: "text", text });
      for (const tc of [...toolCalls.values()].sort((a, b) => a.index - b.index)) {
        let input: unknown = {};
        try {
          input = JSON.parse(tc.function?.arguments || "{}");
        } catch { /* malformed args → empty object */ }
        content.push({
          type: "tool_use",
          id: tc.id ?? crypto.randomUUID(),
          name: tc.function?.name ?? "",
          input,
        });
      }
      // Normalize OpenAI's finish_reason to our stopReason vocabulary.
      const stopReason = finishReason === "tool_calls" ? "tool_use" : finishReason;
      return { content, stopReason, usage };
    },
  };
}

export type Provider = "anthropic" | "openai" | "openrouter";

/**
 * Route a model id to its provider: an "openai:model" id → OpenAI proper, any other
 * "vendor/model" id → OpenRouter, everything else (bare "claude-…") → Anthropic. Pure,
 * so the routing is unit-testable without touching the network.
 */
export function providerFor(model: string): Provider {
  if (model.startsWith("openai:")) return "openai";
  return model.includes("/") ? "openrouter" : "anthropic";
}

/**
 * The LLM client for a model id (see providerFor for the id scheme), wrapped with
 * transient-failure retries. `retry.onRetry` observes re-attempts — the turn
 * runner uses it to reset the UI's streaming buffer and surface the retry.
 */
export function clientFor(model: string, retry?: RetryOpts): LlmClient {
  const inner = ((): LlmClient => {
    switch (providerFor(model)) {
      case "openai":
        return openaiClient();
      case "openrouter":
        return openrouterClient();
      case "anthropic":
        return anthropicClient();
    }
  })();
  return withRetries(inner, retry);
}

/** One-shot text completion — no tools, no event consumer. Returns the
 * concatenated text blocks (untrimmed; callers trim if they care). */
export async function completeText(
  llm: LlmClient,
  opts: { model: string; system: string; maxTokens: number; prompt: string },
): Promise<string> {
  const result = await llm.run(
    {
      model: opts.model,
      system: opts.system,
      maxTokens: opts.maxTokens,
      messages: [{ role: "user", content: [{ type: "text", text: opts.prompt }] }],
      tools: [],
    },
    () => {},
  );
  return result.content
    .filter((b): b is { type: "text"; text: string } => b.type === "text")
    .map((b) => b.text)
    .join("");
}
