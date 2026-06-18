//// HTTP client to a bough server. The TUI is a thin client of the headless
//// server (SPEC.md §3, §8); these calls run inside shore effects so the UI
//// stays responsive while the agent works.

import gleam/dynamic/decode
import gleam/http.{Post}
import gleam/http/request
import gleam/http/response
import gleam/httpc
import gleam/json
import gleam/option
import gleam/result
import gleam/string

pub type ClientError {
  BadUrl
  Transport(String)
  Status(Int, String)
  Decode(String)
}

/// One step in the agent's transcript for a turn. The first three are the
/// generic provider-tool-use shapes; the rest are the supervisor-worker engine's
/// phased events (SPEC §5), which the TUI renders by role.
pub type Step {
  Text(text: String)
  ToolCall(name: String, input: String)
  ToolResult(name: String, output: String)
  Plan(text: String)
  Call(verb: String, arg: String)
  Exec(verb: String, exit: Int, digest: String)
  Worker(command: String, exit: Int)
  Check(ok: Bool, digest: String)
  Review(note: String)
  /// The plan-review gate: a proposed plan awaiting the human's approval. The
  /// run's status is "awaiting_plan" while this is the live tail step.
  Await(plan: String)
}

pub type Reply {
  Reply(text: String, steps: List(Step))
}

/// Progress of a background agent run (polled).
pub type RunState {
  RunState(
    status: String,
    steps: List(Step),
    text: String,
    context_tokens: Int,
  )
}

/// A stored session, for the resume picker.
pub type Summary {
  Summary(id: String, project: String, title: String, turns: Int, updated: Int)
}

/// One node of a session tree.
pub type TreeEntry {
  TreeEntry(
    id: String,
    parent_id: String,
    role: String,
    content: String,
  )
}

pub type Tree {
  Tree(
    id: String,
    project: String,
    active_leaf: String,
    entries: List(TreeEntry),
  )
}

/// GET `/sessions`; the stored sessions, most recent first.
pub fn list_sessions(base: String) -> Result(List(Summary), String) {
  use req <- result.try(
    request.to(base <> "/sessions") |> result.replace_error("invalid server URL"),
  )
  case httpc.send(req) {
    Error(err) -> Error("cannot reach server: " <> string.inspect(err))
    Ok(response.Response(status: 200, body: body, ..)) ->
      json.parse(body, decode.list(summary_decoder()))
      |> result.replace_error("bad sessions response")
    Ok(response.Response(status: code, ..)) ->
      Error("server error " <> string.inspect(code))
  }
}

/// GET `/session/:id`; the full tree.
pub fn get_session(base: String, id: String) -> Result(Tree, String) {
  use req <- result.try(
    request.to(base <> "/session/" <> id)
    |> result.replace_error("invalid server URL"),
  )
  case httpc.send(req) {
    Error(err) -> Error("cannot reach server: " <> string.inspect(err))
    Ok(response.Response(status: 200, body: body, ..)) ->
      json.parse(body, tree_decoder())
      |> result.replace_error("bad session response")
    Ok(response.Response(status: code, ..)) ->
      Error("server error " <> string.inspect(code))
  }
}

/// POST `/session/:id/fork`; repoint the active leaf, returning the new tree.
pub fn fork(base: String, id: String, entry_id: String) -> Result(Tree, String) {
  let body =
    json.to_string(json.object([#("entry_id", json.string(entry_id))]))
  use resp <- result.try(post(base, "/session/" <> id <> "/fork", body) |> describe)
  json.parse(resp, tree_decoder())
  |> result.replace_error("bad fork response")
}

fn summary_decoder() -> decode.Decoder(Summary) {
  use id <- decode.field("id", decode.string)
  use project <- decode.field("project", decode.string)
  use title <- decode.field("title", decode.string)
  use turns <- decode.field("turns", decode.int)
  use updated <- decode.field("updated", decode.int)
  decode.success(Summary(id:, project:, title:, turns:, updated:))
}

fn tree_decoder() -> decode.Decoder(Tree) {
  use id <- decode.field("id", decode.string)
  use project <- decode.field("project", decode.string)
  use active_leaf <- decode.field("active_leaf", decode.optional(decode.string))
  use entries <- decode.field("entries", decode.list(tree_entry_decoder()))
  decode.success(Tree(
    id:,
    project:,
    active_leaf: option.unwrap(active_leaf, ""),
    entries:,
  ))
}

fn tree_entry_decoder() -> decode.Decoder(TreeEntry) {
  use id <- decode.field("id", decode.string)
  use parent_id <- decode.field("parent_id", decode.optional(decode.string))
  use role <- decode.field("role", decode.string)
  use content <- decode.field("content", decode.string)
  decode.success(TreeEntry(
    id:,
    parent_id: option.unwrap(parent_id, ""),
    role:,
    content:,
  ))
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

/// POST `/session/:id/message`; returns the assistant's reply and transcript.
pub fn send_message(
  base: String,
  id: String,
  content: String,
) -> Result(Reply, String) {
  let body = json.to_string(json.object([#("content", json.string(content))]))
  use resp <- result.try(post(base, "/session/" <> id <> "/message", body) |> describe)
  json.parse(resp, reply_decoder())
  |> result.replace_error("bad message response: " <> resp)
}

/// POST `/session/:id/run` to start a background run (returns immediately).
/// `review` turns on the plan-review gate for this run.
pub fn start_run(
  base: String,
  id: String,
  content: String,
  review: Bool,
) -> Result(Nil, String) {
  let body =
    json.to_string(json.object([
      #("content", json.string(content)),
      #("review", json.bool(review)),
    ]))
  post(base, "/session/" <> id <> "/run", body)
  |> describe
  |> result.map(fn(_) { Nil })
}

/// POST `/session/:id/control`: resolve a paused plan (or steer a subagent).
/// `decision` is "allow" or "steer"; `message` carries guidance for a steer.
pub fn send_control(
  base: String,
  id: String,
  decision: String,
  message: String,
) -> Result(Nil, String) {
  let body =
    json.to_string(json.object([
      #("decision", json.string(decision)),
      #("message", json.string(message)),
    ]))
  post(base, "/session/" <> id <> "/control", body)
  |> describe
  |> result.map(fn(_) { Nil })
}

/// GET `/config`; the active supervisor provider and model.
pub fn get_config(base: String) -> Result(#(String, String), String) {
  use req <- result.try(
    request.to(base <> "/config") |> result.replace_error("invalid server URL"),
  )
  case httpc.send(req) {
    Error(err) -> Error("cannot reach server: " <> string.inspect(err))
    Ok(response.Response(status: 200, body: body, ..)) ->
      json.parse(body, config_decoder())
      |> result.replace_error("bad config response")
    Ok(response.Response(status: code, ..)) ->
      Error("server error " <> string.inspect(code))
  }
}

fn config_decoder() -> decode.Decoder(#(String, String)) {
  use provider <- decode.field("provider", decode.string)
  use model <- decode.field("model", decode.string)
  decode.success(#(provider, model))
}

/// GET `/session/:id/run` for current run progress.
pub fn get_run(base: String, id: String) -> Result(RunState, String) {
  use req <- result.try(
    request.to(base <> "/session/" <> id <> "/run")
    |> result.replace_error("invalid server URL"),
  )
  case httpc.send(req) {
    Error(err) -> Error("cannot reach server: " <> string.inspect(err))
    Ok(response.Response(status: 200, body: body, ..)) ->
      json.parse(body, run_state_decoder())
      |> result.replace_error("bad run response")
    Ok(response.Response(status: code, ..)) ->
      Error("server error " <> string.inspect(code))
  }
}

fn run_state_decoder() -> decode.Decoder(RunState) {
  use status <- decode.field("status", decode.string)
  use text <- decode.field("text", decode.string)
  use steps <- decode.field("steps", decode.list(step_decoder()))
  use context_tokens <- decode.optional_field("context_tokens", 0, decode.int)
  decode.success(RunState(
    status: status,
    steps: steps,
    text: text,
    context_tokens: context_tokens,
  ))
}

fn reply_decoder() -> decode.Decoder(Reply) {
  use text <- decode.field("text", decode.string)
  use steps <- decode.field("steps", decode.list(step_decoder()))
  decode.success(Reply(text: text, steps: steps))
}

/// Decode a single run step from a JSON string. Tree entries with the
/// `tool_result` role carry one of these as their content.
pub fn decode_step(content: String) -> Result(Step, Nil) {
  json.parse(content, step_decoder()) |> result.replace_error(Nil)
}

fn step_decoder() -> decode.Decoder(Step) {
  use kind <- decode.field("type", decode.string)
  case kind {
    "text" -> {
      use text <- decode.field("text", decode.string)
      decode.success(Text(text))
    }
    "tool" -> {
      use name <- decode.field("name", decode.string)
      use input <- decode.field("input", decode.string)
      decode.success(ToolCall(name, input))
    }
    "result" -> {
      use name <- decode.field("name", decode.string)
      use output <- decode.field("output", decode.string)
      decode.success(ToolResult(name, output))
    }
    "plan" -> {
      use text <- decode.field("text", decode.string)
      decode.success(Plan(text))
    }
    "call" -> {
      use verb <- decode.field("verb", decode.string)
      use arg <- decode.field("arg", decode.string)
      decode.success(Call(verb, arg))
    }
    "exec" -> {
      use verb <- decode.field("verb", decode.string)
      use exit <- decode.field("exit", decode.int)
      use digest <- decode.field("digest", decode.string)
      decode.success(Exec(verb, exit, digest))
    }
    "worker" -> {
      use command <- decode.field("command", decode.string)
      use exit <- decode.field("exit", decode.int)
      decode.success(Worker(command, exit))
    }
    "check" -> {
      use ok <- decode.field("ok", decode.bool)
      use digest <- decode.field("digest", decode.string)
      decode.success(Check(ok, digest))
    }
    "review" -> {
      use note <- decode.field("note", decode.string)
      decode.success(Review(note))
    }
    "await" -> {
      use plan <- decode.field("plan", decode.string)
      decode.success(Await(plan))
    }
    _ -> decode.failure(Text(""), "Step")
  }
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
