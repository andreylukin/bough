//// Thin HTTP client to a bough server. The TUI is a client of the headless
//// server (SPEC.md §3, §8); this is the connection surface the UI builds on.

import gleam/http/request
import gleam/httpc
import gleam/result
import gleam/string

pub type ClientError {
  BadUrl
  Transport(String)
}

/// GET `<base>/health`; returns the response body on success.
pub fn health(base: String) -> Result(String, ClientError) {
  use req <- result.try(
    request.to(base <> "/health") |> result.replace_error(BadUrl),
  )
  case httpc.send(req) {
    Ok(resp) -> Ok(resp.body)
    Error(err) -> Error(Transport(string.inspect(err)))
  }
}
