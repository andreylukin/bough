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
  let body =
    json.object([
      #("model", json.string(model)),
      #("max_tokens", json.int(max_tokens)),
      #(
        "messages",
        json.preprocessed_array([msg("system", system), msg("user", user)]),
      ),
    ])
    |> json.to_string

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
