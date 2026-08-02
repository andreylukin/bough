/**
 * The provider boundary. Everything that knows Anthropic, OpenAI or OpenRouter
 * exists is in this file, and the only thing that leaves it is `LlmClient`.
 *
 * The invariant: **the turn runner must not know which provider it is talking
 * to.** Three wire protocols, three message encodings, three usage shapes, three
 * ways of admitting that a stream died — all of it collapses to one `run()`. If a
 * provider name, an `openai:` prefix check, or a `cache_control` block appears in
 * any file outside `llm/`, the leak does not stay local: the turn runner, the
 * subagent launcher, the history operations and the TUI all end up branching on
 * provider, and the three paths start drifting independently (plan §8.3).
 *
 * Routing is by model id and nothing else (spec §12):
 *
 *   - `openai:gpt-5`        → OpenAI proper, the Responses API
 *   - `@cf/vendor/model`    → Cloudflare Workers AI, the chat-completions API
 *   - `vendor/model`        → OpenRouter, the chat-completions API
 *   - `claude-opus-5`       → Anthropic, the official SDK
 *
 * `providerFor` is pure, so the routing rule is unit-testable without a key.
 *
 * **Retries are part of the boundary, not part of the runner.** Every client is
 * wrapped in `withRetries`, which is sound because a round has no side effects
 * until `run()` resolves — the turn loop executes tools afterwards — so re-sending
 * identical params can at worst repeat streamed text deltas. That is what
 * `onRetry` is for: the caller resets its streaming buffer and emits
 * `message.retry`.
 *
 * **Nothing here reaches for a global.** The API key reader and `fetch` are
 * injected (defaulting to `process.env` and the global `fetch`), which is what
 * lets `client.test.ts` drive all three encodings offline with a stub transport
 * and no key in sight. Keys are read at `run()` time, not at construction, so a
 * key set through the running server applies without a restart.
 */
import Anthropic from "@anthropic-ai/sdk";
import { LlmError } from "../errors.ts";
import type {
  Effort,
  LlmBlock,
  LlmClient,
  LlmContentBlock,
  LlmMessage,
  LlmParams,
  LlmResult,
  LlmToolDef,
  Usage,
} from "../types.ts";
import { parseToolArgs, sseEvents, throwHttpError } from "./stream.ts";
import { usageCostUsd } from "./pricing.ts";
import { withTrace } from "./trace.ts";
import type { TraceLabel } from "./trace.ts";

// ---- routing ----------------------------------------------------------------

/** The SDK's own `fetch` slot. Named here because `ClientOptions` is not re-exported. */
type AnthropicFetch = NonNullable<NonNullable<ConstructorParameters<typeof Anthropic>[0]>["fetch"]>;

export type Provider = "anthropic" | "openai" | "openrouter" | "cloudflare";

/**
 * Route a model id to its provider: an `openai:model` id → OpenAI proper, a
 * `@cf/…` id → Cloudflare Workers AI, any other `vendor/model` id → OpenRouter,
 * everything else (a bare `claude-…`) → Anthropic. Pure, so the routing is
 * unit-testable without touching the network.
 *
 * Workers AI ids are themselves `vendor/model` shaped (`@cf/meta/llama-…`), so the
 * `@cf/` test HAS to come before the slash test or every Cloudflare model would be
 * sent to OpenRouter — which would answer with a 400 naming a model it never had.
 */
export function providerFor(model: string): Provider {
  if (model.startsWith("openai:")) return "openai";
  if (model.startsWith("@cf/")) return "cloudflare";
  return model.includes("/") ? "openrouter" : "anthropic";
}

/** The env var carrying each provider's key. Read at `run()` time, never cached. */
export const API_KEY_ENV: Record<Provider, string> = {
  anthropic: "ANTHROPIC_API_KEY",
  openai: "OPENAI_API_KEY",
  openrouter: "OPENROUTER_API_KEY",
  cloudflare: "CLOUDFLARE_API_KEY",
};

/**
 * Cloudflare is the one provider whose endpoint is account-scoped: the account id
 * is part of the URL, not a header, so a key alone cannot reach it.
 */
export const CLOUDFLARE_ACCOUNT_ENV = "CLOUDFLARE_ACCOUNT_ID";

/** Reads one environment variable. Injected so tests never depend on the shell. */
export type Env = (key: string) => string | undefined;

const processEnv: Env = (key) => process.env[key];

/** The seams a provider client needs from the outside world. Both are injected. */
export interface ProviderOpts {
  /** Defaults to reading `process.env`. */
  env?: Env;
  /** Defaults to the global `fetch`. Tests pass a stub transport. */
  fetch?: typeof fetch;
}

function requireKey(env: Env, provider: Provider, ...alternatives: string[]): string {
  const names = [API_KEY_ENV[provider], ...alternatives];
  for (const name of names) {
    const value = env(name)?.trim();
    if (value) return value;
  }
  // 401 so `isRetryable` says no: a missing key will still be missing in 15
  // seconds, and six backed-off attempts would only delay the message the user
  // needs to read.
  throw new LlmError(`${provider}: ${names.join(" / ")} is not set`, 401);
}

// ---- transient-failure retries ----------------------------------------------

const retryableStatus = (s: number): boolean => s === 408 || s === 429 || s >= 500;

/**
 * The 400 a chat-completions provider throws when an assistant `tool_calls` is not
 * followed by its matching tool message. `toOpenAIMessages` repairs that encoding
 * itself, so a re-send of the (now well-formed) request succeeds — hence this one
 * 400 is retryable while every other 400 stays fatal, since a real caller mistake
 * must not be retried six times.
 */
export function isToolProtocol400(err: unknown): boolean {
  return err instanceof LlmError && err.status === 400 &&
    /tool_calls|tool_call_id|must be followed by tool/i.test(err.message);
}

/**
 * An error's effective type name.
 *
 * The Anthropic SDK's error classes never set `this.name`, so `.name` reads
 * "Error" for all of them while the console prints the class name. Matching on
 * `.name` alone silently misclassified every SDK connection error as
 * non-retryable, which turned a momentary network flap into an instant turn death.
 * The constructor name is the reliable signal in unbundled source.
 */
export function errName(err: unknown): string {
  const e = err as { name?: string; constructor?: { name?: string } } | null;
  if (e?.name && e.name !== "Error") return e.name;
  return e?.constructor?.name ?? "";
}

/** Should this failure be re-attempted? A user abort and a caller mistake never are. */
export function isRetryable(err: unknown): boolean {
  const name = errName(err);
  if (name === "AbortError" || name === "APIUserAbortError") return false;
  if (err instanceof LlmError) {
    return retryableStatus(err.status) || isToolProtocol400(err);
  }
  // Anthropic SDK: APIError carries `.status`; connection failures carry none.
  const e = err as { status?: unknown } | null;
  if (typeof e?.status === "number") return retryableStatus(e.status);
  if (name === "APIConnectionError" || name === "APIConnectionTimeoutError") return true;
  return err instanceof TypeError; // a fetch network failure
}

export interface RetryOpts {
  /** Observes each re-attempt: called after a retryable failure, before the sleep. */
  onRetry?: (info: { attempt: number; maxAttempts: number; error: Error; delayMs: number }) => void;
  maxAttempts?: number;
  baseDelayMs?: number;
}

/**
 * Six attempts is roughly 15–31s of jittered backoff (1+2+4+8+16s, halved-to-full):
 * long enough to ride out a network-path flap, short enough that a truly dead
 * network fails the turn in under a minute. The sleep is abort-aware, so a user
 * interrupt still cuts it short.
 */
export const MAX_ATTEMPTS = 6;
export const BASE_DELAY_MS = 1000;

/** Transparent retries around an `LlmClient`. See the module comment for why this is sound. */
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

// ---- cost ------------------------------------------------------------------

/**
 * Stamp `costUsd` on a round's usage from the vendored catalog.
 *
 * One wrapper rather than three call sites: the three providers report tokens in
 * three shapes but they all normalize to `Usage` before this runs, so pricing has
 * exactly one implementation and an unpriced model degrades identically on every
 * route (`costUsd: null`, never a silent zero).
 */
export function withPricing(inner: LlmClient): LlmClient {
  return {
    async run(params, onText, signal) {
      const result = await inner.run(params, onText, signal);
      if (!result.usage || result.usage.costUsd != null) return result;
      return {
        ...result,
        usage: { ...result.usage, costUsd: usageCostUsd(params.model, result.usage) },
      };
    },
  };
}

// ---- system-prompt tiers ----------------------------------------------------

/**
 * The two system tiers as Anthropic system blocks, each with a 1-hour cache
 * breakpoint.
 *
 * Order is load-bearing: the API caches everything *before* a breakpoint, so the
 * volatile block must never precede the stable one — one per-session byte early in
 * the prefix defeats cross-session cache sharing entirely. Exported because
 * breakpoint placement IS the cache economics, and a test should be able to say so.
 */
export function anthropicSystemBlocks(
  p: Pick<LlmParams, "system" | "systemVolatile">,
): Anthropic.TextBlockParam[] | undefined {
  const blocks = [p.system, p.systemVolatile]
    .filter((t): t is string => !!t)
    .map((text) => ({
      type: "text" as const,
      text,
      // `ttl` postdates the pinned SDK's types but serializes fine — hence the cast.
      cache_control: { type: "ephemeral", ttl: "1h" } as Anthropic.CacheControlEphemeral,
    }));
  return blocks.length > 0 ? blocks : undefined;
}

/**
 * Both tiers joined, stable first, for the providers that take a single
 * system/instructions field and cache prefixes implicitly. `undefined` when both
 * are empty.
 */
export function joinedSystem(p: Pick<LlmParams, "system" | "systemVolatile">): string | undefined {
  const s = (p.system ?? "") + (p.systemVolatile ?? "");
  return s || undefined;
}

// ---- Anthropic --------------------------------------------------------------

/** Our normalized message → the Anthropic wire shape. Exported for tests. */
export function toApiMessage(m: LlmMessage): Anthropic.MessageParam {
  const content = m.content.flatMap((b: LlmContentBlock): Anthropic.ContentBlockParam[] => {
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
        // A thinking block replays verbatim, signature included — the API rejects
        // a tool_use whose preceding thinking was altered or dropped.
        const meta = b.meta as { type?: string } | undefined;
        if (meta?.type === "thinking" || meta?.type === "redacted_thinking") {
          return [meta as Anthropic.ContentBlockParam];
        }
        // Foreign reasoning degrades to prose; an empty text block is rejected.
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
      return undefined; // server tools etc. — we do not enable those features
  }
}

/**
 * Thinking depth as request params: adaptive thinking with summarized display, so
 * the UI's reasoning folds carry text.
 *
 * Guarded by model, because adaptive thinking exists only on the Claude 5 family
 * and Opus 4.8+ — sending it to e.g. Haiku 4.5 is a hard 400, and a per-session
 * effort setting must not kill a turn just because the user switched models.
 * Returned as a plain object and spread in, since the pinned SDK predates these
 * fields in its types but serializes unknown request keys fine.
 */
export function effortParams(effort?: Effort, model?: string): Record<string, unknown> {
  const supported = model === undefined ||
    /claude-(fable|mythos|sonnet|opus)-5|opus-4-[89]/.test(model);
  return effort && supported
    ? { thinking: { type: "adaptive", display: "summarized" }, output_config: { effort } }
    : {};
}

/**
 * The Anthropic route, on the official SDK.
 *
 * Prompt caching uses three breakpoints (longer TTLs must precede shorter ones;
 * the budget is four):
 *
 *   - the STABLE system block at a 1-hour TTL — that prefix is byte-identical
 *     across sessions, so it warms new sessions and survives a lunch break;
 *   - the VOLATILE system block, also 1h — caches across turns within a session
 *     without splintering the shared prefix;
 *   - the final block of the final message, at the default 5-minute sliding TTL —
 *     extends the cached conversation prefix each round, since the API reuses the
 *     longest previously cached prefix.
 *
 * `maxRetries: 0`: the retry policy is `withRetries`, uniform across providers.
 * The SDK's own pre-stream retries would stack underneath it and turn six attempts
 * into eighteen.
 */
export function anthropicClient(opts: ProviderOpts = {}): LlmClient {
  const env = opts.env ?? processEnv;
  return {
    async run(params, onText, signal) {
      const apiKey = requireKey(env, "anthropic", "ANTHROPIC_AUTH_TOKEN");
      const client = new Anthropic({
        apiKey,
        maxRetries: 0,
        ...(opts.fetch ? { fetch: opts.fetch as unknown as AnthropicFetch } : {}),
      });
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
      stream.on("text", (delta: string) => onText(delta));
      const final = await stream.finalMessage();
      const content = final.content
        .map(fromApiBlock)
        .filter((b): b is LlmBlock => b !== undefined);
      const cacheRead = final.usage.cache_read_input_tokens ?? 0;
      const cacheWrite = final.usage.cache_creation_input_tokens ?? 0;
      const usage: Usage = {
        // `input_tokens` is the uncached remainder; add reads and writes back so
        // the context meter shows the true prompt size.
        inputTokens: final.usage.input_tokens + cacheRead + cacheWrite,
        outputTokens: final.usage.output_tokens,
        cacheReadTokens: cacheRead,
        cacheWriteTokens: cacheWrite,
      };
      return { content, stopReason: final.stop_reason ?? "end_turn", usage };
    },
  };
}

// ---- OpenAI proper: the Responses API ---------------------------------------
//
// Chat/completions cannot combine function tools with reasoning on the gpt-5/o*
// families, so OpenAI rides /v1/responses. Stateless (`store: false`): each round
// replays the whole history as input items, with reasoning items — their encrypted
// content requested via `include` — echoed back verbatim before their
// function_call, because the API rejects a function_call whose reasoning item is
// missing. Those items ride `LlmBlock.meta` through the turn's in-memory round
// loop; across turns the replay mapper drops them and old function_calls replay
// bare, which is accepted, since the pairing rule binds items of the live chain.

/** `LlmMessage[]` → Responses `input` items. Exported for tests. */
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

interface ResponsesFinal {
  output?: ResponsesItem[];
  status?: string;
  incomplete_details?: { reason?: string };
  error?: { code?: string; message?: string };
  usage?: {
    input_tokens?: number;
    output_tokens?: number;
    input_tokens_details?: { cached_tokens?: number };
    output_tokens_details?: { reasoning_tokens?: number };
  };
}

/**
 * A Responses `output` array → our normalized blocks. `tools` is what lets a
 * truncated function call be told apart from a legitimately argument-less one.
 * Exported for tests.
 */
export function fromResponsesOutput(
  output: ResponsesItem[],
  tools: LlmToolDef[] = [],
): LlmBlock[] {
  const blocks: LlmBlock[] = [];
  for (const item of output) {
    if (item.type === "message") {
      const text = (item.content ?? [])
        .filter((c) => c.type === "output_text")
        .map((c) => c.text ?? "")
        .join("");
      if (text) blocks.push({ type: "text", text });
    } else if (item.type === "function_call") {
      const name = item.name ?? "";
      const input = parseToolArgs(
        "openai",
        item.arguments,
        tools.find((t) => t.name === name),
        name,
      );
      blocks.push({ type: "tool_use", id: item.call_id ?? "", name, input });
    } else if (item.type === "reasoning") {
      const text = (item.summary ?? []).map((s) => s.text ?? "").join("\n");
      blocks.push({ type: "reasoning", text, meta: item });
    }
  }
  return blocks;
}

export function openaiClient(opts: ProviderOpts = {}): LlmClient {
  const env = opts.env ?? processEnv;
  const doFetch = opts.fetch ?? fetch;
  return {
    async run(params, onText, signal) {
      const apiKey = requireKey(env, "openai");
      const base = env("OPENAI_API_BASE") ?? "https://api.openai.com";
      // The picker id carries the routing prefix; the wire wants the bare model.
      const model = params.model.startsWith("openai:")
        ? params.model.slice("openai:".length)
        : params.model;
      const res = await doFetch(`${base}/v1/responses`, {
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
          // The Responses API caps reasoning effort at "high".
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
      // response.completed payload, so there is no per-item assembly to get wrong.
      let final: ResponsesFinal | undefined;
      for await (const data of sseEvents(res.body, "openai")) {
        if (data === "[DONE]") continue;
        let ev: { type?: string; delta?: string; response?: ResponsesFinal };
        try {
          ev = JSON.parse(data);
        } catch {
          continue;
        }
        if (ev.type === "response.output_text.delta" && ev.delta) onText(ev.delta);
        else if (ev.type === "response.completed" && ev.response) final = ev.response;
        else if ((ev.type === "response.failed" || ev.type === "error") && !final) {
          // A mid-stream failure event is server-side (the request itself was
          // accepted), so it classifies retryable; rate limits by their code.
          const code = ev.response?.error?.code ?? "";
          throw new LlmError(
            `openai: ${JSON.stringify(ev)}`,
            code.includes("rate_limit") ? 429 : 500,
          );
        } else if (ev.type === "response.incomplete" && ev.response) final = ev.response;
      }
      // No terminal status at all → the stream was cut → a transport fault.
      if (!final) throw new LlmError("openai: stream ended without response.completed");

      const content = fromResponsesOutput(final.output ?? [], params.tools);
      const stopReason = content.some((b) => b.type === "tool_use")
        ? "tool_use"
        : final.status === "incomplete" && final.incomplete_details?.reason === "max_output_tokens"
        ? "max_tokens"
        : "end_turn";
      const usage: Usage = {
        inputTokens: final.usage?.input_tokens ?? 0,
        outputTokens: final.usage?.output_tokens ?? 0,
        reasoningTokens: final.usage?.output_tokens_details?.reasoning_tokens ?? 0,
        cacheReadTokens: final.usage?.input_tokens_details?.cached_tokens ?? 0,
        cacheWriteTokens: 0,
      };
      return { content, stopReason, usage };
    },
  };
}

// ---- OpenRouter: the chat-completions shape ---------------------------------

interface OpenAIToolCall {
  index: number;
  id?: string;
  function?: { name?: string; arguments?: string };
}

/**
 * Flatten our multi-block messages into chat-completions messages, splitting
 * tool_results out into their own `tool` messages.
 *
 * The repair pass at the end is not optional. Every assistant `tool_calls` id MUST
 * be followed by a matching `{role:"tool", tool_call_id}` before the next non-tool
 * message, or the provider rejects the whole request with a 400 — including the
 * case where an interrupt left the transcript with a call and no result. A
 * synthesized `(interrupted)` result keeps the request well-formed no matter what
 * history assembly handed us. Exported for tests.
 */
export function toOpenAIMessages(system: string | undefined, messages: LlmMessage[]): unknown[] {
  const out: unknown[] = [];
  if (system) out.push({ role: "system", content: system });
  for (const m of messages) {
    if (m.role === "assistant") {
      const text = m.content
        .filter((b) => b.type === "text")
        .map((b) => (b as { text: string }).text)
        .join("");
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
      // A user turn: text/image blocks become one user message; each tool_result
      // becomes its own tool message. With no images the content stays a plain
      // string (wire shape unchanged); with images it becomes the multimodal parts
      // array, which a non-vision model rejects — surfaced as-is.
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
  const repaired: unknown[] = [];
  for (let i = 0; i < out.length; i++) {
    const msg = out[i] as { role: string; tool_calls?: { id: string }[] };
    repaired.push(msg);
    if (msg.role !== "assistant" || !msg.tool_calls?.length) continue;
    const provided = new Set<string>();
    for (let j = i + 1; j < out.length; j++) {
      const t = out[j] as { role: string; tool_call_id?: string };
      if (t.role !== "tool") break;
      if (t.tool_call_id) provided.add(t.tool_call_id);
    }
    for (const call of msg.tool_calls) {
      if (!provided.has(call.id)) {
        repaired.push({ role: "tool", tool_call_id: call.id, content: "(interrupted)" });
      }
    }
  }
  return repaired;
}

interface OpenAICompatOpts extends ProviderOpts {
  /** Names the provider in error text. */
  provider: Provider;
  /**
   * A function when the URL depends on the environment (Cloudflare's account id is
   * part of the path), so it is resolved at `run()` time like the key is — a value
   * set through the running server must apply without a restart.
   */
  url: string | ((env: Env) => string);
  extraHeaders?: Record<string, string>;
  /** Env vars accepted for the key besides `API_KEY_ENV[provider]`. */
  keyAlternatives?: string[];
}

/**
 * The chat-completions streaming client behind OpenRouter. A separate family from
 * the Anthropic client with a base-URL override, because it speaks a different
 * message and tool encoding end to end.
 */
function openAICompatClient(opts: OpenAICompatOpts): LlmClient {
  const env = opts.env ?? processEnv;
  const doFetch = opts.fetch ?? fetch;
  return {
    async run(params, onText, signal) {
      const apiKey = requireKey(env, opts.provider, ...(opts.keyAlternatives ?? []));
      const url = typeof opts.url === "string" ? opts.url : opts.url(env);
      const res = await doFetch(url, {
        method: "POST",
        signal,
        headers: {
          authorization: `Bearer ${apiKey}`,
          "content-type": "application/json",
          ...opts.extraHeaders,
        },
        body: JSON.stringify({
          model: params.model,
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
      let usage: Usage | undefined;
      // Whether the stream reached a proper end ([DONE] or a finish_reason). A
      // stream that merely closes was cut mid-response, and returning the partial
      // round as success would run half-assembled tool calls.
      let ended = false;

      for await (const data of sseEvents(res.body, opts.provider)) {
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
            completion_tokens_details?: { reasoning_tokens?: number };
          };
        };
        try {
          chunk = JSON.parse(data);
        } catch {
          continue;
        }
        // An upstream provider failure arrives as a terminal `error` chunk on an
        // otherwise-200 stream; without this the partial round passes as success.
        if (chunk.error) {
          throw new LlmError(
            `${opts.provider}: ${chunk.error.message ?? JSON.stringify(chunk.error)}`,
            typeof chunk.error.code === "number" ? chunk.error.code : 502,
          );
        }
        if (chunk.usage) {
          usage = {
            inputTokens: chunk.usage.prompt_tokens ?? 0,
            outputTokens: chunk.usage.completion_tokens ?? 0,
            reasoningTokens: chunk.usage.completion_tokens_details?.reasoning_tokens ?? 0,
            // The upstream provider's cache hits, relayed in the OpenAI shape.
            cacheReadTokens: chunk.usage.prompt_tokens_details?.cached_tokens ?? 0,
            cacheWriteTokens: 0,
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
      if (!ended) throw new LlmError(`${opts.provider}: stream truncated before completion`);

      const content: LlmBlock[] = [];
      if (text) content.push({ type: "text", text });
      for (const tc of [...toolCalls.values()].sort((a, b) => a.index - b.index)) {
        const name = tc.function?.name ?? "";
        content.push({
          type: "tool_use",
          id: tc.id ?? crypto.randomUUID(),
          name,
          input: parseToolArgs(
            opts.provider,
            tc.function?.arguments,
            params.tools.find((t) => t.name === name),
            name,
          ),
        });
      }
      // Normalize the finish_reason vocabulary to ours.
      const stopReason = finishReason === "tool_calls"
        ? "tool_use"
        : finishReason === "length"
        ? "max_tokens"
        : finishReason;
      return { content, stopReason, usage };
    },
  };
}

export function openrouterClient(opts: ProviderOpts = {}): LlmClient {
  return openAICompatClient({
    ...opts,
    provider: "openrouter",
    url: "https://openrouter.ai/api/v1/chat/completions",
    extraHeaders: { "x-title": "bough" },
  });
}

// ---- Cloudflare Workers AI: chat-completions, account-scoped ----------------

/** The account-scoped Workers AI base, overridable for a gateway or a test server. */
function cloudflareBase(env: Env): string {
  const account = env(CLOUDFLARE_ACCOUNT_ENV)?.trim();
  if (!account) {
    // 401 for the same reason a missing key is: it will still be missing in 15
    // seconds, so six backed-off attempts only delay the message that fixes it.
    throw new LlmError(`cloudflare: ${CLOUDFLARE_ACCOUNT_ENV} is not set`, 401);
  }
  const base = env("CLOUDFLARE_API_BASE") ?? "https://api.cloudflare.com/client/v4";
  return `${base}/accounts/${account}/ai`;
}

/**
 * Workers AI over its OpenAI-compatible endpoint.
 *
 * Cloudflare serves `/ai/v1/chat/completions` in the chat-completions shape, so it
 * reuses the OpenRouter family wholesale; the only thing that differs is that the
 * account id lives in the path, which is why the URL is a function of the env.
 */
export function cloudflareClient(opts: ProviderOpts = {}): LlmClient {
  return openAICompatClient({
    ...opts,
    provider: "cloudflare",
    url: (env) => `${cloudflareBase(env)}/v1/chat/completions`,
    // Cloudflare's own docs and dashboard call it a token, so accept that spelling.
    keyAlternatives: ["CLOUDFLARE_API_TOKEN"],
  });
}

// ---- the factory ------------------------------------------------------------

export interface ClientOpts extends ProviderOpts {
  retry?: RetryOpts;
  /**
   * Record raw provider I/O for this turn. Null or absent = no tracing and no
   * wrapper (`llm/trace.ts`).
   */
  trace?: TraceLabel | null;
}

/** The bare provider client for a model id, without retries or pricing. */
export function providerClient(model: string, opts: ProviderOpts = {}): LlmClient {
  switch (providerFor(model)) {
    case "openai":
      return openaiClient(opts);
    case "openrouter":
      return openrouterClient(opts);
    case "cloudflare":
      return cloudflareClient(opts);
    case "anthropic":
      return anthropicClient(opts);
  }
}

/**
 * **The only entry point the rest of the tree uses.** Routes a model id to its
 * provider (see `providerFor`), prices the round from the vendored catalog, and
 * wraps the whole thing in transient-failure retries. `retry.onRetry` observes
 * re-attempts — the turn runner uses it to reset the streaming buffer and emit
 * `message.retry`.
 */
export function clientFor(model: string, opts: ClientOpts = {}): LlmClient {
  // Tracing sits INSIDE the retries so a recorded trace shows each attempt, and
  // outside pricing so a recorded round already carries its cost (`llm/trace.ts`).
  return withRetries(withTrace(withPricing(providerClient(model, opts)), opts.trace ?? null), opts.retry);
}

/**
 * One-shot text completion: no tools, no event consumer. Used by the cheap tier
 * and by the history operations that need a summary. Returns the concatenated text
 * blocks untrimmed; callers trim if they care.
 */
export async function completeText(
  llm: LlmClient,
  opts: { model: string; system: string; maxTokens: number; prompt: string },
): Promise<string> {
  const result: LlmResult = await llm.run(
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

// ---- the model catalog ------------------------------------------------------
//
// Model ids live here for the same reason everything else does: an id IS a
// provider routing decision, so a picker entry written anywhere else would put a
// provider name outside `llm/`.

export interface ModelRow {
  id: string;
  label: string;
  provider: Provider;
}

/** The curated picker entries. Frontier and cheap tiers are both chosen here (spec §12). */
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
  { id: "@cf/zai-org/glm-5.2", label: "GLM 5.2 (Workers AI)", provider: "cloudflare" },
  { id: "@cf/openai/gpt-oss-120b", label: "GPT-OSS 120B (Workers AI)", provider: "cloudflare" },
  {
    id: "@cf/moonshotai/kimi-k2.7-code",
    label: "Kimi K2.7 Code (Workers AI)",
    provider: "cloudflare",
  },
  {
    id: "@cf/meta/llama-3.3-70b-instruct-fp8-fast",
    label: "Llama 3.3 70B (Workers AI)",
    provider: "cloudflare",
  },
];

// Chat models only: completions/embeddings/audio/image ids would either 404 on the
// Responses API or make no sense in a coding-agent picker. Dated snapshots are
// dropped — the alias id always exists and tracks the latest.
const OPENAI_INCLUDE = /^(gpt-|o\d|chatgpt-)/;
const OPENAI_EXCLUDE =
  /audio|realtime|tts|whisper|embed|dall|image|moderation|transcribe|search-preview|instruct/;
const OPENAI_DATED = /-\d{4}-\d{2}-\d{2}$/;
const OPENAI_CAP = 25;

/**
 * Newest-first, comparing version numbers as NUMBERS.
 *
 * A plain descending `localeCompare` reads "gpt-5.10" as older than "gpt-5.6", because
 * "1" sorts before "6" one character at a time. That is invisible until a family reaches
 * a double-digit minor, and then it is the newest model in the list that sinks below the
 * cap and vanishes from the picker — the exact failure this discovery code exists to
 * prevent. Digit runs are therefore compared numerically and everything else
 * lexicographically, which leaves ids without numbers ordered exactly as before.
 */
function byNewest(a: string, b: string): number {
  const split = (s: string) => s.split(/(\d+)/);
  const [as, bs] = [split(a), split(b)];
  for (let i = 0; i < Math.max(as.length, bs.length); i++) {
    const [x, y] = [as[i] ?? "", bs[i] ?? ""];
    if (x === y) continue;
    // Both numeric only when the id shape matched this far; a number against a word is
    // still a string comparison, which is what keeps `o3` above `gpt-5`.
    const numeric = /^\d+$/.test(x) && /^\d+$/.test(y);
    return numeric ? Number(y) - Number(x) : y.localeCompare(x);
  }
  return 0;
}

/** Pure filter/mapper, so the selection rules are testable without the network. */
export function filterOpenAIModels(ids: string[]): ModelRow[] {
  return ids
    .filter((id) => OPENAI_INCLUDE.test(id) && !OPENAI_EXCLUDE.test(id) && !OPENAI_DATED.test(id))
    .sort(byNewest)
    .slice(0, OPENAI_CAP)
    .map((id) => ({ id: `openai:${id}`, label: `${id} (OpenAI)`, provider: "openai" as const }));
}

/** Static table first, discovered entries after, deduped by id. */
export function mergeModels(staticModels: ModelRow[], dynamic: ModelRow[]): ModelRow[] {
  const seen = new Set(staticModels.map((m) => m.id));
  return [...staticModels, ...dynamic.filter((m) => !seen.has(m.id))];
}

/**
 * One GET, parsed as `{data: [...]}`, or an empty list.
 *
 * Every provider's model endpoint answers that shape, so the differences that
 * remain are the URL, the auth header, and how a row becomes a `ModelRow` — which
 * is exactly what each caller passes in. Sharing the failure policy matters more
 * than sharing the shape: discovery is a picker nicety, so **no key, a bad key, a
 * rate limit or an offline machine all mean "no extra rows"**, never a thrown
 * error, and never a slow boot (`server/models.ts` races this against a deadline).
 */
async function fetchModelList<T>(
  url: string,
  headers: Record<string, string>,
  doFetch: typeof fetch,
  map: (rows: T[]) => ModelRow[],
): Promise<ModelRow[]> {
  try {
    const res = await doFetch(url, { headers, signal: AbortSignal.timeout(10_000) });
    if (!res.ok) return [];
    const body = await res.json() as { data?: T[] };
    return map(body.data ?? []);
  } catch {
    return [];
  }
}

/**
 * Ask OpenAI what it offers, for the picker. **Never throws and never caches**: no
 * key, a bad key or an offline machine simply yields an empty list and the static
 * table still works. The caller owns any caching — a module-level cache here would
 * be exactly the global this file is written to avoid.
 */
export async function discoverOpenAIModels(opts: ProviderOpts = {}): Promise<ModelRow[]> {
  const env = opts.env ?? processEnv;
  const key = env(API_KEY_ENV.openai)?.trim();
  if (!key) return [];
  const base = env("OPENAI_API_BASE") ?? "https://api.openai.com";
  return fetchModelList<{ id?: unknown }>(
    `${base}/v1/models`,
    { authorization: `Bearer ${key}` },
    opts.fetch ?? fetch,
    (rows) =>
      filterOpenAIModels(
        rows.map((m) => m.id).filter((id): id is string => typeof id === "string"),
      ),
  );
}

/**
 * Ask Anthropic what it offers. Same failure policy as the OpenAI path.
 *
 * `display_name` is used verbatim when present — the API already names its models
 * the way a human would ("Claude Opus 4.8"), so inventing a label here would be a
 * second naming scheme to keep in sync with theirs. Ids are bare, which is what
 * `providerFor` routes to Anthropic, so nothing is prefixed.
 */
export async function discoverAnthropicModels(opts: ProviderOpts = {}): Promise<ModelRow[]> {
  const env = opts.env ?? processEnv;
  const key = env(API_KEY_ENV.anthropic)?.trim();
  if (!key) return [];
  const base = env("ANTHROPIC_API_BASE") ?? "https://api.anthropic.com";
  return fetchModelList<{ id?: unknown; display_name?: unknown }>(
    `${base}/v1/models?limit=1000`,
    { "x-api-key": key, "anthropic-version": "2023-06-01" },
    opts.fetch ?? fetch,
    (rows) =>
      rows.flatMap((m) =>
        typeof m.id === "string"
          ? [{
            id: m.id,
            label: typeof m.display_name === "string" && m.display_name ? m.display_name : m.id,
            provider: "anthropic" as const,
          }]
          : []
      ),
  );
}

/**
 * Ask OpenRouter what it offers.
 *
 * The one provider whose catalog is PUBLIC — `/api/v1/models` answers without a
 * key. The key is still sent when there is one (it scopes the list to what the
 * account can actually reach), but its absence is not a reason to skip the call:
 * a user deciding whether to add an OpenRouter key is better served by seeing what
 * they would get. This is also the list that makes a search box necessary rather
 * than a nicety — it is hundreds of rows, where the others are tens.
 */
export async function discoverOpenRouterModels(opts: ProviderOpts = {}): Promise<ModelRow[]> {
  const env = opts.env ?? processEnv;
  const key = env(API_KEY_ENV.openrouter)?.trim();
  const base = env("OPENROUTER_API_BASE") ?? "https://openrouter.ai/api";
  return fetchModelList<{ id?: unknown; name?: unknown }>(
    `${base}/v1/models`,
    key ? { authorization: `Bearer ${key}` } : {},
    opts.fetch ?? fetch,
    (rows) =>
      rows.flatMap((m) =>
        typeof m.id === "string"
          ? [{
            id: m.id,
            label: typeof m.name === "string" && m.name ? m.name : m.id,
            provider: "openrouter" as const,
          }]
          : []
      ),
  );
}

/**
 * Ask Cloudflare what its account can run.
 *
 * Two things make this one not reuse `fetchModelList`: the catalog is
 * account-scoped (no account id, no list — same failure policy as no key: an empty
 * list, never a throw), and Workers AI answers `{result: [...]}` rather than
 * `{data: [...]}`. The task filter is the point of the call — the catalog is mostly
 * embeddings, image and speech models, none of which belong in a model picker.
 */
export async function discoverCloudflareModels(opts: ProviderOpts = {}): Promise<ModelRow[]> {
  const env = opts.env ?? processEnv;
  const key = env(API_KEY_ENV.cloudflare)?.trim() ?? env("CLOUDFLARE_API_TOKEN")?.trim();
  const account = env(CLOUDFLARE_ACCOUNT_ENV)?.trim();
  if (!key || !account) return [];
  const doFetch = opts.fetch ?? fetch;
  const url = `${
    env("CLOUDFLARE_API_BASE") ?? "https://api.cloudflare.com/client/v4"
  }/accounts/${account}/ai/models/search?task=Text+Generation&per_page=100&hide_experimental=true`;
  try {
    const res = await doFetch(url, {
      headers: { authorization: `Bearer ${key}` },
      signal: AbortSignal.timeout(10_000),
    });
    if (!res.ok) return [];
    const body = await res.json() as { result?: { name?: unknown; description?: unknown }[] };
    return (body.result ?? []).flatMap((m) =>
      typeof m.name === "string" && m.name.startsWith("@cf/")
        ? [{ id: m.name, label: `${m.name.slice("@cf/".length)} (Workers AI)`, provider:
          "cloudflare" as const }]
        : []
    );
  } catch {
    return [];
  }
}

/**
 * Every provider at once. **Concurrent and independently fallible**: one provider
 * being down, keyless or slow must not cost the others their rows, which is what
 * `allSettled` buys over `all` — and each discovery already resolves to `[]` rather
 * than rejecting, so the settled check is the belt to that braces.
 */
export async function discoverModels(opts: ProviderOpts = {}): Promise<ModelRow[]> {
  const results = await Promise.allSettled([
    discoverAnthropicModels(opts),
    discoverOpenAIModels(opts),
    discoverOpenRouterModels(opts),
    discoverCloudflareModels(opts),
  ]);
  return results.flatMap((r) => (r.status === "fulfilled" ? r.value : []));
}
