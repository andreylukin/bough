//// Supervisor provider abstraction (SPEC §5.5). The engine speaks one neutral
//// shape; this module translates to/from each provider's wire format so the
//// run_steps tool-use loop works the same whether the supervisor is Anthropic
//// (Messages API, content blocks + tool_use) or an OpenAI-compatible endpoint
//// like OpenRouter (chat completions, tool_calls + role:"tool" results).

import bough_server/anthropic
import bough_server/json_value.{type JsonValue, JArray, JObject, JString}
import bough_server/openai
import gleam/json
import gleam/list
import gleam/result
import gleam/string

pub type Provider {
  Anthropic
  /// Any OpenAI-compatible chat-completions endpoint (e.g. OpenRouter), given
  /// its base URL up to and including `/v1`.
  OpenAICompat(base_url: String)
}

/// A normalized tool call: `input` is the decoded arguments object.
pub type ToolUse {
  ToolUse(id: String, name: String, input: JsonValue)
}

pub type Response {
  Response(
    /// The assistant turn to append to the conversation verbatim (carries the
    /// provider's tool-call blocks so a following tool result matches it).
    assistant: JsonValue,
    text: String,
    tool_uses: List(ToolUse),
    stop_reason: String,
    input_tokens: Int,
    output_tokens: Int,
  )
}

/// Call the supervisor with the conversation and the single `tool` (name +
/// description + JSON-schema body), wrapped in the provider's tool format.
pub fn complete(
  p: Provider,
  api_key: String,
  model: String,
  system: String,
  messages: List(JsonValue),
  tool_name: String,
  tool_description: String,
  tool_schema: json.Json,
) -> Result(Response, String) {
  case p {
    Anthropic -> {
      let tools =
        json.preprocessed_array([
          json.object([
            #("name", json.string(tool_name)),
            #("description", json.string(tool_description)),
            #("input_schema", tool_schema),
          ]),
        ])
      use r <- result.try(anthropic.complete(api_key, model, system, messages, tools))
      Ok(Response(
        assistant: JObject([
          #("role", JString("assistant")),
          #("content", JArray(r.content)),
        ]),
        text: r.text,
        tool_uses: list.map(r.tool_uses, fn(t) { ToolUse(t.id, t.name, t.input) }),
        stop_reason: r.stop_reason,
        input_tokens: r.input_tokens,
        output_tokens: r.output_tokens,
      ))
    }
    OpenAICompat(base_url) -> {
      let tools =
        json.preprocessed_array([
          json.object([
            #("type", json.string("function")),
            #("function", json.object([
              #("name", json.string(tool_name)),
              #("description", json.string(tool_description)),
              #("parameters", tool_schema),
            ])),
          ]),
        ])
      use r <- result.try(openai.complete(
        base_url,
        api_key,
        model,
        system,
        messages,
        tools,
      ))
      Ok(Response(
        assistant: r.message,
        text: oai_text(r.message),
        tool_uses: oai_tool_uses(r.message),
        stop_reason: r.finish_reason,
        input_tokens: r.prompt_tokens,
        output_tokens: r.completion_tokens,
      ))
    }
  }
}

// --- Conversation message builders (provider-specific) --------------------

/// Both providers accept a plain string `content` for user/assistant turns.
pub fn user_text(text: String) -> JsonValue {
  JObject([#("role", JString("user")), #("content", JString(text))])
}

pub fn assistant_text(text: String) -> JsonValue {
  JObject([#("role", JString("assistant")), #("content", JString(text))])
}

/// The result of a tool call, in the shape the provider expects to follow the
/// assistant turn that requested it.
pub fn tool_result(p: Provider, tool_use_id: String, content: String) -> JsonValue {
  case p {
    Anthropic ->
      JObject([
        #("role", JString("user")),
        #("content", JArray([
          JObject([
            #("type", JString("tool_result")),
            #("tool_use_id", JString(tool_use_id)),
            #("content", JString(content)),
          ]),
        ])),
      ])
    OpenAICompat(_) ->
      JObject([
        #("role", JString("tool")),
        #("tool_call_id", JString(tool_use_id)),
        #("content", JString(content)),
      ])
  }
}

/// The assistant text of a stored message (Anthropic: text content blocks;
/// OpenAI: a string `content`).
pub fn message_text(p: Provider, message: JsonValue) -> String {
  case p {
    Anthropic ->
      case json_value.field(message, "content") {
        Ok(JArray(blocks)) ->
          blocks
          |> list.filter_map(fn(b) {
            case json_value.string_field(b, "type") {
              Ok("text") -> json_value.string_field(b, "text")
              _ -> Error(Nil)
            }
          })
          |> string.join("\n")
        Ok(JString(s)) -> s
        _ -> ""
      }
    OpenAICompat(_) ->
      case json_value.field(message, "content") {
        Ok(JString(s)) -> s
        _ -> ""
      }
  }
}

// --- OpenAI assistant-message extraction ----------------------------------

fn oai_text(message: JsonValue) -> String {
  case json_value.field(message, "content") {
    Ok(JString(s)) -> s
    _ -> ""
  }
}

fn oai_tool_uses(message: JsonValue) -> List(ToolUse) {
  case json_value.field(message, "tool_calls") {
    Ok(JArray(calls)) ->
      list.filter_map(calls, fn(c) {
        use id <- result.try(json_value.string_field(c, "id"))
        use func <- result.try(json_value.field(c, "function"))
        use name <- result.try(json_value.string_field(func, "name"))
        use args <- result.try(json_value.string_field(func, "arguments"))
        // `arguments` is a JSON string; decode it to the input object.
        let input =
          json.parse(args, json_value.decoder()) |> result.unwrap(JObject([]))
        Ok(ToolUse(id, name, input))
      })
    _ -> []
  }
}
