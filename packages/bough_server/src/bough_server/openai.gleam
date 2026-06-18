//// OpenAI-compatible Chat Completions client (SPEC §5.5) — the second
//// supervisor provider, used for OpenRouter (and any other OpenAI-shaped
//// endpoint). Messages round-trip as `JsonValue` so an assistant turn (incl.
//// its `tool_calls`) can be echoed back verbatim, like the Anthropic client.

import bough_server/json_value.{type JsonValue, JObject, JString}
import gleam/dynamic/decode
import gleam/http.{Post}
import gleam/http/request
import gleam/http/response
import gleam/httpc
import gleam/int
import gleam/json
import gleam/list
import gleam/result
import gleam/string

const max_tokens = 4096

// Slow reasoning models behind OpenRouter, plus a conversation that grows each
// round, can run well past httpc's 30s default.
const request_timeout = 300_000

pub type Response {
  Response(
    /// The raw assistant `message` object, to echo back in the next request.
    message: JsonValue,
    finish_reason: String,
    prompt_tokens: Int,
    completion_tokens: Int,
  )
}

pub fn complete(
  base_url: String,
  api_key: String,
  model: String,
  system: String,
  messages: List(JsonValue),
  tools: json.Json,
) -> Result(Response, String) {
  // OpenAI carries the system prompt as the first message, not a top-level field.
  let all = [JObject([#("role", JString("system")), #("content", JString(system))]), ..messages]
  let body =
    json.object([
      #("model", json.string(model)),
      #("max_tokens", json.int(max_tokens)),
      #("messages", json.preprocessed_array(list.map(all, json_value.to_json))),
      #("tools", tools),
      #("tool_choice", json.string("auto")),
    ])
    |> json.to_string

  use base <- result.try(
    request.to(base_url <> "/chat/completions")
    |> result.replace_error("invalid provider url: " <> base_url),
  )
  let req =
    base
    |> request.set_method(Post)
    |> request.set_header("content-type", "application/json")
    |> request.set_header("authorization", "Bearer " <> api_key)
    |> request.set_body(body)

  case httpc.configure() |> httpc.timeout(request_timeout) |> httpc.dispatch(req) {
    Error(e) -> Error("http error: " <> string.inspect(e))
    Ok(response.Response(status: 200, body: b, ..)) -> parse_response(b)
    Ok(response.Response(status: code, body: b, ..)) ->
      Error("provider " <> int.to_string(code) <> ": " <> string.trim(b))
  }
}

fn parse_response(body: String) -> Result(Response, String) {
  let usage_decoder = {
    use prompt <- decode.optional_field("prompt_tokens", 0, decode.int)
    use completion <- decode.optional_field("completion_tokens", 0, decode.int)
    decode.success(#(prompt, completion))
  }
  let choice_decoder = {
    use message <- decode.field("message", json_value.decoder())
    use finish <- decode.optional_field("finish_reason", "stop", decode.string)
    decode.success(#(message, finish))
  }
  let decoder = {
    use choices <- decode.field("choices", decode.list(choice_decoder))
    use usage <- decode.optional_field("usage", #(0, 0), usage_decoder)
    decode.success(#(choices, usage))
  }
  // OpenRouter can answer 200 with an `{"error": {...}}` body (e.g. an upstream
  // 504), often after keep-alive whitespace — surface that message, not the raw
  // padded body.
  case json.parse(body, error_decoder()) {
    Ok(message) -> Error("provider error: " <> message)
    Error(_) ->
      case json.parse(body, decoder) {
        Ok(#([#(message, finish), ..], #(prompt, completion))) ->
          Ok(Response(message, finish, prompt, completion))
        Ok(#([], _)) -> Error("provider returned no choices")
        Error(_) ->
          Error("could not parse provider response: " <> string.trim(body))
      }
  }
}

fn error_decoder() -> decode.Decoder(String) {
  use message <- decode.subfield(["error", "message"], decode.string)
  decode.success(message)
}
