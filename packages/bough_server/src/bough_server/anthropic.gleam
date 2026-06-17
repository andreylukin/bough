//// Anthropic Messages API client with tool use (SPEC.md §5). The first
//// concrete provider; the agent loop talks to it. Messages are carried as
//// `JsonValue` so assistant turns (incl. tool_use blocks) round-trip verbatim.

import bough_server/json_value.{type JsonValue}
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

const api_url = "https://api.anthropic.com/v1/messages"

const anthropic_version = "2023-06-01"

const max_tokens = 4096

pub type ToolUse {
  ToolUse(id: String, name: String, input: JsonValue)
}

pub type Response {
  Response(
    /// Raw assistant content blocks, to echo back in the next request.
    content: List(JsonValue),
    text: String,
    tool_uses: List(ToolUse),
    stop_reason: String,
  )
}

pub fn complete(
  api_key: String,
  model: String,
  system: String,
  messages: List(JsonValue),
  tools: json.Json,
) -> Result(Response, String) {
  let body =
    json.object([
      #("model", json.string(model)),
      #("max_tokens", json.int(max_tokens)),
      #("system", json.string(system)),
      #(
        "messages",
        json.preprocessed_array(list.map(messages, json_value.to_json)),
      ),
      #("tools", tools),
    ])
    |> json.to_string

  use base <- result.try(
    request.to(api_url) |> result.replace_error("invalid api url"),
  )
  let req =
    base
    |> request.set_method(Post)
    |> request.set_header("content-type", "application/json")
    |> request.set_header("x-api-key", api_key)
    |> request.set_header("anthropic-version", anthropic_version)
    |> request.set_body(body)

  case httpc.send(req) {
    Error(e) -> Error("http error: " <> string.inspect(e))
    Ok(response.Response(status: 200, body: body, ..)) -> parse_response(body)
    Ok(response.Response(status: code, body: body, ..)) ->
      Error("anthropic " <> int.to_string(code) <> ": " <> body)
  }
}

fn parse_response(body: String) -> Result(Response, String) {
  let decoder = {
    use stop_reason <- decode.field("stop_reason", decode.string)
    use content <- decode.field("content", decode.list(json_value.decoder()))
    decode.success(#(stop_reason, content))
  }
  use parsed <- result.try(
    json.parse(body, decoder)
    |> result.replace_error("could not parse anthropic response: " <> body),
  )
  let #(stop_reason, content) = parsed
  Ok(Response(
    content: content,
    text: extract_text(content),
    tool_uses: extract_tool_uses(content),
    stop_reason: stop_reason,
  ))
}

fn extract_text(content: List(JsonValue)) -> String {
  content
  |> list.filter_map(fn(block) {
    case json_value.string_field(block, "type") {
      Ok("text") -> json_value.string_field(block, "text")
      _ -> Error(Nil)
    }
  })
  |> string.join("\n")
}

fn extract_tool_uses(content: List(JsonValue)) -> List(ToolUse) {
  content
  |> list.filter_map(fn(block) {
    case json_value.string_field(block, "type") {
      Ok("tool_use") -> {
        use id <- result.try(json_value.string_field(block, "id"))
        use name <- result.try(json_value.string_field(block, "name"))
        use input <- result.try(json_value.field(block, "input"))
        Ok(ToolUse(id, name, input))
      }
      _ -> Error(Nil)
    }
  })
}
