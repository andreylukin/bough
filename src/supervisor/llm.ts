/**
 * The LLM boundary. turn.ts speaks only in the normalized shapes below; the real
 * Anthropic SDK lives behind `anthropicClient` and nowhere else. Tests inject a
 * scripted fake implementing `LlmClient`, so the runner never touches the network.
 *
 * Normalization choices:
 *   - Content blocks are text / tool_use / reasoning / tool_result. We collapse the
 *     SDK's block zoo to these; anything else (redacted_thinking, server tools) is
 *     dropped, which is safe because we don't enable those features.
 *   - We do NOT enable extended thinking (the `thinking` param is omitted). So no
 *     `reasoning` blocks come back in practice, and there are no thinking-block
 *     replay constraints inside the tool loop. `reasoning` stays in the union only
 *     to carry historical parts (which the history mapper drops before replay).
 */
import Anthropic from "@anthropic-ai/sdk";

export type LlmBlock =
  | { type: "text"; text: string }
  | { type: "reasoning"; text: string }
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

export interface LlmParams {
  model: string;
  system?: string;
  maxTokens: number;
  messages: LlmMessage[];
  tools: LlmToolDef[];
}

/** Token usage for one round; summed across a turn for the context meter. */
export interface LlmUsage {
  inputTokens: number;
  outputTokens: number;
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
  const content = m.content.map((b): Anthropic.ContentBlockParam => {
    switch (b.type) {
      case "text":
      case "reasoning":
        return { type: "text", text: b.text };
      case "tool_use":
        return { type: "tool_use", id: b.id, name: b.name, input: b.input ?? {} };
      case "tool_result":
        return {
          type: "tool_result",
          tool_use_id: b.toolUseId,
          content: b.content,
          is_error: b.isError,
        };
    }
  });
  return { role: m.role, content };
}

function fromApiBlock(block: Anthropic.ContentBlock): LlmBlock | undefined {
  switch (block.type) {
    case "text":
      return { type: "text", text: block.text };
    case "thinking":
      return { type: "reasoning", text: block.thinking };
    case "tool_use":
      return { type: "tool_use", id: block.id, name: block.name, input: block.input };
    default:
      return undefined; // redacted_thinking, server tools, etc. — not used here
  }
}

export function anthropicClient(): LlmClient {
  const client = new Anthropic();
  return {
    async run(params, onText, signal) {
      const stream = client.messages.stream({
        model: params.model,
        max_tokens: params.maxTokens,
        system: params.system,
        messages: params.messages.map(toApiMessage),
        tools: params.tools.map((t) => ({
          name: t.name,
          description: t.description,
          input_schema: t.inputSchema as Anthropic.Tool.InputSchema,
        })),
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
      };
      return { content, stopReason: final.stop_reason ?? "end_turn", usage };
    },
  };
}

// ---- OpenRouter (OpenAI-compatible) ---------------------------------------
//
// A separate provider, not the Anthropic client with a base_url override: it speaks
// the OpenAI chat-completions shape (different message/tool encoding), so it gets its
// own hand-rolled fetch client. Selected when the model id carries a provider prefix
// (e.g. "anthropic/claude-3.5-sonnet", "openai/gpt-4o"). Needs OPENROUTER_API_KEY.

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
      const text = m.content.filter((b) => b.type === "text").map((b) => (b as { text: string }).text).join("");
      const toolCalls = m.content
        .filter((b) => b.type === "tool_use")
        .map((b) => {
          const t = b as { id: string; name: string; input: unknown };
          return { id: t.id, type: "function", function: { name: t.name, arguments: JSON.stringify(t.input ?? {}) } };
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
        out.push({ role: "user", content: texts.map((b) => (b as { text: string }).text).join("\n") });
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
  const apiKey = Deno.env.get("OPENROUTER_API_KEY");
  return {
    async run(params, onText, signal) {
      if (!apiKey) throw new Error("OPENROUTER_API_KEY is not set");
      const res = await fetch("https://openrouter.ai/api/v1/chat/completions", {
        method: "POST",
        signal,
        headers: {
          authorization: `Bearer ${apiKey}`,
          "content-type": "application/json",
          "x-title": "bough",
        },
        body: JSON.stringify({
          model: params.model,
          max_tokens: params.maxTokens,
          stream: true,
          stream_options: { include_usage: true },
          messages: toOpenAIMessages(params.system, params.messages),
          tools: params.tools.map((t) => ({
            type: "function",
            function: { name: t.name, description: t.description, parameters: t.inputSchema },
          })),
        }),
      });
      if (!res.ok || !res.body) {
        throw new Error(`openrouter: ${res.status} ${await res.text().catch(() => "")}`);
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
            choices?: { delta?: { content?: string; tool_calls?: OpenAIToolCall[] }; finish_reason?: string }[];
            usage?: { prompt_tokens?: number; completion_tokens?: number };
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
              cur.function = { ...cur.function, arguments: (cur.function?.arguments ?? "") + tc.function.arguments };
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
        content.push({ type: "tool_use", id: tc.id ?? crypto.randomUUID(), name: tc.function?.name ?? "", input });
      }
      // Normalize OpenAI's finish_reason to our stopReason vocabulary.
      const stopReason = finishReason === "tool_calls" ? "tool_use" : finishReason;
      return { content, stopReason, usage };
    },
  };
}

/** Pick the client for a model id: a provider-prefixed id ("x/y") → OpenRouter, else Anthropic. */
export function clientFor(model: string): LlmClient {
  return model.includes("/") ? openrouterClient() : anthropicClient();
}
