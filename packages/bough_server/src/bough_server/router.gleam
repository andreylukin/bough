//// HTTP + SSE routes. Published as an OpenAPI spec at `/doc` so clients and
//// SDKs can be generated (opencode-style, SPEC.md §8).
////
//// Live routes:
////   GET  /                     service banner
////   GET  /health               liveness
////   GET  /doc                  OpenAPI placeholder
////   POST /session              create a session            -> session JSON
////   GET  /session/:id          fetch a session             -> session JSON
////   POST /session/:id/entry    append an entry (persisted)  -> entry JSON
////
//// The message/fork/events routes (agent loop, SSE) land next (SPEC.md §10).

import bough_core
import bough_core/session.{type SessionTree, Entry}
import bough_server/clock
import bough_server/session_manager
import gleam/dynamic/decode
import gleam/http.{Get, Post}
import gleam/json
import gleam/option.{type Option, None, Some}
import gleam/result
import wisp.{type Request, type Response}

pub fn handle_request(req: Request) -> Response {
  case wisp.path_segments(req), req.method {
    [], _ -> json_ok("{\"service\":\"bough\",\"version\":\"" <> bough_core.version <> "\"}")
    ["health"], _ -> json_ok("{\"status\":\"ok\"}")
    ["doc"], _ -> doc()
    ["session"], Post -> create_session(req)
    ["session", id], Get -> get_session(id)
    ["session", id, "entry"], Post -> add_entry(req, id)
    _, _ -> wisp.not_found()
  }
}

fn doc() -> Response {
  json_ok(
    "{\"openapi\":\"3.1.0\",\"info\":{\"title\":\"bough\",\"version\":\""
    <> bough_core.version
    <> "\"},\"paths\":{}}",
  )
}

// --- Sessions ------------------------------------------------------------

fn create_session(req: Request) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, create_decoder()) {
    Error(_) -> wisp.bad_request("expected optional {\"project\": string}")
    Ok(project) -> {
      let tree = session.new(wisp.random_string(16), project)
      case session_manager.save(tree) {
        Ok(_) -> created(json.to_string(session.tree_to_json(tree)))
        Error(_) -> wisp.internal_server_error()
      }
    }
  }
}

fn get_session(id: String) -> Response {
  case session_manager.load(id) {
    Ok(tree) -> json_ok(json.to_string(session.tree_to_json(tree)))
    Error(_) -> wisp.not_found()
  }
}

fn add_entry(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, entry_req_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"role\": string, \"content\": string}")
    Ok(er) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) -> persist_entry(tree, er)
      }
  }
}

fn persist_entry(tree: SessionTree, er: EntryReq) -> Response {
  let role = session.role_from_string(er.role) |> result.unwrap(session.User)
  let parent = case er.parent_id {
    Some(_) -> er.parent_id
    None -> tree.active_leaf
  }
  let entry =
    Entry(
      id: wisp.random_string(16),
      parent_id: parent,
      role: role,
      content: er.content,
      snapshot_ref: None,
      label: None,
      timestamp: clock.now_ms(),
    )
  case session_manager.save(session.append(tree, entry)) {
    Ok(_) -> created(json.to_string(session.entry_to_json(entry)))
    Error(_) -> wisp.internal_server_error()
  }
}

// --- Request bodies ------------------------------------------------------

fn create_decoder() -> decode.Decoder(String) {
  use project <- decode.optional_field("project", "default", decode.string)
  decode.success(project)
}

type EntryReq {
  EntryReq(role: String, content: String, parent_id: Option(String))
}

fn entry_req_decoder() -> decode.Decoder(EntryReq) {
  use role <- decode.field("role", decode.string)
  use content <- decode.field("content", decode.string)
  use parent_id <- decode.optional_field(
    "parent_id",
    None,
    decode.optional(decode.string),
  )
  decode.success(EntryReq(role: role, content: content, parent_id: parent_id))
}

// --- Helpers -------------------------------------------------------------

fn json_ok(body: String) -> Response {
  wisp.json_response(body, 200)
}

fn created(body: String) -> Response {
  wisp.json_response(body, 201)
}
