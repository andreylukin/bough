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
 */
import Anthropic from "@anthropic-ai/sdk";

export type LlmBlock =
  | { type: "text"; text: string }
  // `meta` is an opaque provider payload replayed verbatim within a turn — the
  // OpenAI Responses API requires a function_call's reasoning item (encrypted
  // content included) to precede it in the next round's input.
  | { type: "reasoning"; text: string; meta?: unknown }
  | { type: "tool_use"; id: string; name: string; input: unknown };

/** A block as it appears in a request message (adds tool_result to LlmBlock). */
export type LlmContentBlock =
  | LlmBlock
  | { type: "tool_result"; toolUseId: string; content: string; isError: boolean };

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
  system?: string;
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

// ---- real client -----------------------------------------------------------

function toApiMessage(m: LlmMessage): Anthropic.MessageParam {
  const content = m.content.flatMap((b): Anthropic.ContentBlockParam[] => {
    switch (b.type) {
      case "text":
        return [{ type: "text", text: b.text }];
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
function effortParams(effort?: Effort): Record<string, unknown> {
  return effort
    ? {
      thinking: { type: "adaptive", display: "summarized" },
      output_config: { effort },
    }
    : {};
}

export function anthropicClient(): LlmClient {
  const client = new Anthropic();
  return {
    async run(params, onText, signal) {
      // Prompt caching (5-minute sliding TTL, refreshed free on every hit):
      //   - breakpoint on the system block caches tools + system together;
      //   - breakpoint on the final block of the final message extends the cached
      //     prefix each round (the API reuses the longest previously cached prefix).
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
        system: params.system
          ? [{ type: "text", text: params.system, cache_control: { type: "ephemeral" } }]
          : undefined,
        messages,
        tools: params.tools.map((t) => ({
          name: t.name,
          description: t.description,
          input_schema: t.inputSchema as Anthropic.Tool.InputSchema,
        })),
        ...(params.toolChoice ? { tool_choice: { type: params.toolChoice } } : {}),
        ...effortParams(params.effort),
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

/** Flatten our multi-block messages into OpenAI chat messages (tool_results split out). */
function toOpenAIMessages(system: string | undefined, messages: LlmMessage[]): unknown[] {
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
      // user turn: text blocks → one user message; tool_result blocks → one tool msg each.
      const texts = m.content.filter((b) => b.type === "text");
      const results = m.content.filter((b) => b.type === "tool_result");
      if (texts.length) {
        out.push({
          role: "user",
          content: texts.map((b) => (b as { text: string }).text).join("\n"),
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
          instructions: params.system,
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
      if (!res.ok || !res.body) {
        throw new Error(`openai: ${res.status} ${await res.text().catch(() => "")}`);
      }

      // Deltas stream for the live feel; the final content comes whole from the
      // response.completed payload (no per-item assembly to get wrong).
      let final: {
        output?: ResponsesItem[];
        status?: string;
        incomplete_details?: { reason?: string };
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
        const { done, value } = await reader.read();
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
            throw new Error(`openai: ${JSON.stringify(ev)}`);
          } else if (ev.type === "response.incomplete" && ev.response) final = ev.response;
        }
      }
      if (!final) throw new Error("openai: stream ended without response.completed");

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
          messages: toOpenAIMessages(params.system, params.messages),
          tools: params.tools.map((t) => ({
            type: "function",
            function: { name: t.name, description: t.description, parameters: t.inputSchema },
          })),
          ...(params.toolChoice ? { tool_choice: params.toolChoice } : {}),
        }),
      });
      if (!res.ok || !res.body) {
        throw new Error(`${opts.provider}: ${res.status} ${await res.text().catch(() => "")}`);
      }

      let text = "";
      const toolCalls = new Map<number, OpenAIToolCall>();
      let finishReason = "stop";
      let usage: LlmUsage | undefined;

      const reader = res.body.getReader();
      const dec = new TextDecoder();
      let buffer = "";
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        buffer += dec.decode(value, { stream: true });
        let nl: number;
        while ((nl = buffer.indexOf("\n")) >= 0) {
          const line = buffer.slice(0, nl).trim();
          buffer = buffer.slice(nl + 1);
          if (!line.startsWith("data:")) continue;
          const data = line.slice(5).trim();
          if (data === "[DONE]") continue;
          let chunk: {
            choices?: {
              delta?: { content?: string; tool_calls?: OpenAIToolCall[] };
              finish_reason?: string;
            }[];
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
          if (choice.finish_reason) finishReason = choice.finish_reason;
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

/** The LLM client for a model id (see providerFor for the id scheme). */
export function clientFor(model: string): LlmClient {
  switch (providerFor(model)) {
    case "openai":
      return openaiClient();
    case "openrouter":
      return openrouterClient();
    case "anthropic":
      return anthropicClient();
  }
}
