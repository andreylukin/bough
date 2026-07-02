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

export interface LlmResult {
  content: LlmBlock[];
  stopReason: string;
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
      return { content, stopReason: final.stop_reason ?? "end_turn" };
    },
  };
}
