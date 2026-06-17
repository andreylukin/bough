//// HTTP + SSE routes. Published as an OpenAPI spec at `/doc` so clients and
//// SDKs can be generated (opencode-style, SPEC.md §8).
////
//// Only `/` and `/health` are live in the slice; the session/fork/events
//// routes are reserved (SPEC.md §8, §11).

import bough_core
import wisp.{type Request, type Response}

pub fn handle_request(req: Request) -> Response {
  case wisp.path_segments(req) {
    [] -> root()
    ["health"] -> health()
    ["doc"] -> doc()
    _ -> wisp.not_found()
  }
}

fn root() -> Response {
  json("{\"service\":\"bough\",\"version\":\"" <> bough_core.version <> "\"}")
}

fn health() -> Response {
  json("{\"status\":\"ok\"}")
}

/// Placeholder OpenAPI 3.1 document. Filled in as routes land (SPEC.md §8).
fn doc() -> Response {
  json(
    "{\"openapi\":\"3.1.0\",\"info\":{\"title\":\"bough\",\"version\":\""
    <> bough_core.version
    <> "\"},\"paths\":{}}",
  )
}

fn json(body: String) -> Response {
  wisp.json_response(body, 200)
}
