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
import bough_core/session.{type Entry, type SessionTree, Entry}
import bough_server/agent
import bough_server/clock
import bough_server/session_manager
import envoy
import gleam/dynamic/decode
import gleam/http.{Get, Post}
import gleam/json
import gleam/option.{type Option, None, Some}
import gleam/result
import wisp.{type Request, type Response}

const system_prompt = "You are bough, a coding agent operating inside a sandboxed workspace. Use the tools to accomplish the user's task: `bash` runs in a sandbox with no network and workspace read/write; `read`/`write`/`edit` manage files. Prefer absolute paths under the workspace. Be concise."

const default_model = "claude-sonnet-4-6"

pub fn handle_request(req: Request) -> Response {
  case wisp.path_segments(req), req.method {
    [], _ -> json_ok("{\"service\":\"bough\",\"version\":\"" <> bough_core.version <> "\"}")
    ["health"], _ -> json_ok("{\"status\":\"ok\"}")
    ["doc"], _ -> doc()
    ["session"], Post -> create_session(req)
    ["session", id], Get -> get_session(id)
    ["session", id, "entry"], Post -> add_entry(req, id)
    ["session", id, "message"], Post -> send_message(req, id)
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

// --- Agent loop ----------------------------------------------------------

fn send_message(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, content_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"content\": string}")
    Ok(content) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) -> run_agent(tree, content)
      }
  }
}

fn run_agent(tree: SessionTree, content: String) -> Response {
  let user = make_entry(session.User, content, tree.active_leaf)
  let tree = session.append(tree, user)

  case envoy.get("ANTHROPIC_API_KEY") {
    Error(_) -> json_error("ANTHROPIC_API_KEY is not set")
    Ok(api_key) -> {
      let model = envoy.get("BOUGH_MODEL") |> result.unwrap(default_model)
      case agent.run(api_key, model, tree.project, system_prompt, content) {
        Error(message) -> {
          let _ = session_manager.save(tree)
          json_error(message)
        }
        Ok(outcome) -> {
          let assistant =
            make_entry(session.Assistant, outcome.text, Some(user.id))
          let tree = session.append(tree, assistant)
          case session_manager.save(tree) {
            Ok(_) -> created(json.to_string(session.entry_to_json(assistant)))
            Error(_) -> wisp.internal_server_error()
          }
        }
      }
    }
  }
}

fn make_entry(
  role: session.Role,
  content: String,
  parent: Option(String),
) -> Entry {
  Entry(
    id: wisp.random_string(16),
    parent_id: parent,
    role: role,
    content: content,
    snapshot_ref: None,
    label: None,
    timestamp: clock.now_ms(),
  )
}

// --- Request bodies ------------------------------------------------------

fn content_decoder() -> decode.Decoder(String) {
  use content <- decode.field("content", decode.string)
  decode.success(content)
}

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

fn json_error(message: String) -> Response {
  wisp.json_response(
    json.to_string(json.object([#("error", json.string(message))])),
    500,
  )
}
