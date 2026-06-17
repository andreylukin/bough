//// HTTP client to a bough server. The TUI is a thin client of the headless
//// server (SPEC.md §3, §8); these calls run inside shore effects so the UI
//// stays responsive while the agent works.

import gleam/dynamic/decode
import gleam/http.{Post}
import gleam/http/request
import gleam/http/response
import gleam/httpc
import gleam/json
import gleam/result
import gleam/string

pub type ClientError {
  BadUrl
  Transport(String)
  Status(Int, String)
  Decode(String)
}

/// GET `<base>/health`; returns the response body on success.
pub fn health(base: String) -> Result(String, ClientError) {
  use req <- result.try(
    request.to(base <> "/health") |> result.replace_error(BadUrl),
  )
  case httpc.send(req) {
    Ok(response.Response(status: 200, body: body, ..)) -> Ok(body)
    Ok(response.Response(status: code, body: body, ..)) ->
      Error(Status(code, body))
    Error(err) -> Error(Transport(string.inspect(err)))
  }
}

/// POST `/session` with a project path; returns the new session id.
pub fn create_session(base: String, project: String) -> Result(String, String) {
  let body = json.to_string(json.object([#("project", json.string(project))]))
  use resp <- result.try(post(base, "/session", body) |> describe)
  json.parse(resp, string_field("id"))
  |> result.replace_error("bad create-session response: " <> resp)
}

/// POST `/session/:id/message`; returns the assistant's reply text.
pub fn send_message(
  base: String,
  id: String,
  content: String,
) -> Result(String, String) {
  let body = json.to_string(json.object([#("content", json.string(content))]))
  use resp <- result.try(post(base, "/session/" <> id <> "/message", body) |> describe)
  json.parse(resp, string_field("content"))
  |> result.replace_error("bad message response: " <> resp)
}

// --- internals -----------------------------------------------------------

fn post(base: String, path: String, body: String) -> Result(String, ClientError) {
  use base_req <- result.try(
    request.to(base <> path) |> result.replace_error(BadUrl),
  )
  let req =
    base_req
    |> request.set_method(Post)
    |> request.set_header("content-type", "application/json")
    |> request.set_body(body)

  case httpc.send(req) {
    Error(err) -> Error(Transport(string.inspect(err)))
    Ok(response.Response(status: status, body: response_body, ..)) ->
      case status >= 200 && status < 300 {
        True -> Ok(response_body)
        False -> Error(Status(status, response_body))
      }
  }
}

fn string_field(name: String) -> decode.Decoder(String) {
  use value <- decode.field(name, decode.string)
  decode.success(value)
}

fn describe(result: Result(a, ClientError)) -> Result(a, String) {
  result.map_error(result, fn(err) {
    case err {
      BadUrl -> "invalid server URL"
      Transport(m) -> "cannot reach server: " <> m
      Status(code, body) -> "server error " <> string.inspect(code) <> ": " <> body
      Decode(m) -> "decode error: " <> m
    }
  })
}
