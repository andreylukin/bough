//// The worker provider (SPEC.md §5.1, §5.5): a small local model that proposes
//// one fix command when a step fails. Just a second provider speaking the
//// OpenAI-compatible `/v1/chat/completions` shape — the same contract whether
//// it is served by bough's bundled `llama-server` (§5.6), a llamafile, or a
//// remote endpoint.

import gleam/dynamic/decode
import gleam/http.{Post}
import gleam/http/request
import gleam/http/response
import gleam/httpc
import gleam/int
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string

/// Send one system+user exchange and return the assistant's text.
pub fn complete(
  base_url: String,
  model: String,
  system: String,
  user: String,
  max_tokens: Int,
) -> Result(String, String) {
  complete_with(base_url, model, system, user, max_tokens, None, None)
}

/// Like `complete`, but with explicit sampling. Reasoning-tuned workers (e.g.
/// VibeThinker-3B) require their recommended decoding — temperature 1.0, top_p
/// 0.95 — and lowering temperature degrades them; pass them here. `None` leaves
/// the field off so the server's default applies (matching `complete`).
pub fn complete_with(
  base_url: String,
  model: String,
  system: String,
  user: String,
  max_tokens: Int,
  temperature: Option(Float),
  top_p: Option(Float),
) -> Result(String, String) {
  let fields = [
    #("model", json.string(model)),
    #("max_tokens", json.int(max_tokens)),
    #(
      "messages",
      json.preprocessed_array([msg("system", system), msg("user", user)]),
    ),
  ]
  let fields = case temperature {
    Some(t) -> list.append(fields, [#("temperature", json.float(t))])
    None -> fields
  }
  let fields = case top_p {
    Some(p) -> list.append(fields, [#("top_p", json.float(p))])
    None -> fields
  }
  let body = json.object(fields) |> json.to_string

  use base <- result.try(
    request.to(base_url <> "/v1/chat/completions")
    |> result.replace_error("invalid worker url: " <> base_url),
  )
  let req =
    base
    |> request.set_method(Post)
    |> request.set_header("content-type", "application/json")
    |> request.set_body(body)

  case httpc.send(req) {
    Error(e) -> Error("worker http error: " <> string.inspect(e))
    Ok(response.Response(status: 200, body: b, ..)) -> parse(b)
    Ok(response.Response(status: code, body: b, ..)) ->
      Error("worker " <> int.to_string(code) <> ": " <> b)
  }
}

fn msg(role: String, content: String) -> json.Json {
  json.object([#("role", json.string(role)), #("content", json.string(content))])
}

fn parse(body: String) -> Result(String, String) {
  let message_decoder = {
    use content <- decode.field("content", decode.string)
    decode.success(content)
  }
  let choice_decoder = {
    use message <- decode.field("message", message_decoder)
    decode.success(message)
  }
  let decoder = {
    use choices <- decode.field("choices", decode.list(choice_decoder))
    decode.success(choices)
  }
  case json.parse(body, decoder) {
    Ok([first, ..]) -> Ok(first)
    Ok([]) -> Error("worker returned no choices")
    Error(_) -> Error("could not parse worker response: " <> body)
  }
}
