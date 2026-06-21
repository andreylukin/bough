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
import bough_server/control
import bough_server/engine
import bough_server/net_profile
import bough_server/provider
import bough_server/run_store
import bough_server/session_manager
import bough_server/snapshots
import bough_server/subagents
import bough_server/worker_runtime
import envoy
import gleam/dynamic/decode
import gleam/erlang/process
import gleam/http.{Get, Post}
import gleam/int
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/result
import wisp.{type Request, type Response}

const default_model = "claude-haiku-4-5-20251001"

const default_openrouter_model = "z-ai/glm-5.2"

const openrouter_base = "https://openrouter.ai/api/v1"

const default_max_turns = 20

const default_worker_port = 8080

// The only worker model bough uses: Qwen2.5-Coder, served locally via
// llama-server. This is the label sent to the OpenAI-compatible endpoint;
// llama-server serves whatever GGUF it was started with (see worker_runtime).
const worker_model = "qwen2.5-coder"

pub fn handle_request(req: Request) -> Response {
  case wisp.path_segments(req), req.method {
    [], _ ->
      json_ok(
        "{\"service\":\"bough\",\"version\":\"" <> bough_core.version <> "\"}",
      )
    ["health"], _ -> json_ok("{\"status\":\"ok\"}")
    ["config"], Get -> config()
    ["doc"], _ -> doc()
    ["sessions"], Get -> list_sessions()
    ["session"], Post -> create_session(req)
    ["session", id], Get -> get_session(id)
    ["session", id, "entry"], Post -> add_entry(req, id)
    ["session", id, "message"], Post -> send_message(req, id)
    ["session", id, "run"], Post -> start_run(req, id)
    ["session", id, "run"], Get -> get_run(id)
    ["session", id, "control"], Post -> control_run(req, id)
    ["session", id, "subagents"], Get -> subagents_of(id)
    ["session", id, "fork"], Post -> fork_session(req, id)
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

/// The active supervisor provider and model, so clients can show what's in use.
fn config() -> Response {
  let #(name, model) = resolved_model()
  json_ok(
    json.to_string(
      json.object([
        #("provider", json.string(name)),
        #("model", json.string(model)),
      ]),
    ),
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
    Error(_) ->
      wisp.bad_request("expected {\"role\": string, \"content\": string}")
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
  let history = history_of(tree)
  let user = make_entry(session.User, content, tree.active_leaf)
  let tree = session.append(tree, user)

  case agent_setup() {
    Error(m) -> json_error(m)
    Ok(#(prov, api_key, model)) -> {
      case
        engine.run(
          api_key,
          model,
          tree.project,
          engine_config(prov, False, False),
          history,
          content,
        )
      {
        Error(message) -> {
          let _ = session_manager.save(tree)
          json_error(message)
        }
        Ok(outcome) -> {
          let snap = capture_snapshot(tree.id, tree.project)
          let tree = append_turn(tree, outcome, snap)
          case session_manager.save(tree) {
            Ok(_) ->
              created(
                json.to_string(agent.run_json(
                  "done",
                  outcome.steps,
                  outcome.text,
                  outcome.context_tokens,
                  outcome.net_events,
                )),
              )
            Error(_) -> wisp.internal_server_error()
          }
        }
      }
    }
  }
}

fn max_rounds() -> Int {
  case envoy.get("BOUGH_MAX_TURNS") {
    Ok(v) -> int.parse(v) |> result.unwrap(default_max_turns)
    Error(_) -> default_max_turns
  }
}

/// Engine config from the environment. The worker (Qwen2.5-Coder, SPEC.md §5.6)
/// is always enabled: bough ensures a local llama-server is up and points the
/// worker at it. Set `BOUGH_WORKER_URL` to use a remote endpoint instead
/// (honored inside `worker_runtime.ensure`).
fn engine_config(
  prov: provider.Provider,
  review: Bool,
  net_gate: Bool,
) -> engine.Config {
  let base = engine.default_config()
  let worker_url =
    worker_runtime.ensure(worker_port()) |> result.unwrap(base.worker_url)
  engine.Config(
    ..base,
    provider: prov,
    worker: Some(worker_model),
    worker_url: worker_url,
    max_rounds: max_rounds(),
    review: review,
    net_gate: net_gate,
    net_credentials: net_credentials(),
  )
}

/// The network leash is opt-in: with `BOUGH_NET=1`, the agent's commands get
/// default-deny network with per-host approval; otherwise the network is fully
/// blocked, as before.
fn net_gate() -> Bool {
  envoy.get("BOUGH_NET") |> result.is_ok
}

/// Opt-in credential injection for sandboxed commands (SPEC §6.4): set
/// `BOUGH_NET_CREDENTIALS` to a comma-separated list of `name=ENV_VAR` (or bare
/// `name`). Only entries whose env var is actually set are declared, so the
/// generated profile never references a missing credential.
fn net_credentials() -> List(#(String, String)) {
  case envoy.get("BOUGH_NET_CREDENTIALS") {
    Error(_) -> []
    Ok(spec) ->
      net_profile.parse_credentials(spec)
      |> list.filter(fn(c) { envoy.get(c.1) |> result.is_ok })
  }
}

/// The resolved supervisor provider name and model from the environment
/// (no key required, so `/config` can report it before any run). Defaults to
/// OpenRouter / z-ai/glm-5.2; set `BOUGH_PROVIDER=anthropic` for Anthropic.
fn resolved_model() -> #(String, String) {
  case envoy.get("BOUGH_PROVIDER") {
    Ok("anthropic") -> #(
      "anthropic",
      envoy.get("BOUGH_MODEL") |> result.unwrap(default_model),
    )
    _ -> #(
      "openrouter",
      envoy.get("BOUGH_MODEL") |> result.unwrap(default_openrouter_model),
    )
  }
}

/// Pick the supervisor provider, API key, and model from the environment.
/// Default is OpenRouter (OPENROUTER_API_KEY, model z-ai/glm-5.2);
/// `BOUGH_PROVIDER=anthropic` uses Anthropic with ANTHROPIC_API_KEY.
fn agent_setup() -> Result(#(provider.Provider, String, String), String) {
  let #(name, model) = resolved_model()
  case name {
    "anthropic" -> {
      use key <- result.try(
        envoy.get("ANTHROPIC_API_KEY")
        |> result.replace_error("ANTHROPIC_API_KEY is not set"),
      )
      Ok(#(provider.Anthropic, key, model))
    }
    _ -> {
      use key <- result.try(
        envoy.get("OPENROUTER_API_KEY")
        |> result.replace_error("OPENROUTER_API_KEY is not set"),
      )
      Ok(#(provider.OpenAICompat(openrouter_base), key, model))
    }
  }
}

/// Block the running engine until the human resolves a paused plan, polling the
/// control slot. Capped (~10 min at 250ms/poll) so an abandoned run can't pin a
/// process forever — a timeout steers the supervisor to pause rather than act.
fn await_decision(id: String, polls: Int) -> control.Decision {
  case control.take(id) {
    Ok(decision) -> decision
    Error(_) ->
      case polls > 2400 {
        True ->
          control.Steer("Approval timed out. Stop and wait for the human.")
        False -> {
          process.sleep(250)
          await_decision(id, polls + 1)
        }
      }
  }
}

/// A pending human message for a run, consumed non-blocking (only `Steer`
/// messages; a stray `Allow` outside the review gate is ignored).
fn inbox_of(id: String) -> Option(String) {
  case control.take(id) {
    Ok(control.Steer(message)) -> Some(message)
    _ -> None
  }
}

/// GET `/session/:id/subagents`: the children this session has spawned.
fn subagents_of(id: String) -> Response {
  json_ok(json.to_string(subagents.to_json(subagents.list(id))))
}

/// The subagent operations for a parent session: spawn (async), tell (message a
/// running child), collect (wait for a child's result). Wired recursively so a
/// subagent gets the same operations over its own children.
fn subagents_for(
  parent_id: String,
  prov: provider.Provider,
  api_key: String,
  model: String,
  workspace: String,
) -> engine.Subagents {
  engine.Subagents(
    spawn: fn(title, task) {
      spawn_subagent(parent_id, prov, api_key, model, workspace, title, task)
    },
    tell: fn(target, message) {
      control.put(target, control.Steer(message))
      "Message queued for subagent " <> target <> "."
    },
    collect: fn(target) { collect_subagent(target) },
  )
}

/// Start a subagent running concurrently and return its id immediately (SPEC
/// §5). The child is a fresh session on the same workspace with its own
/// run/control slots, so the parent (via tell/collect) and the human (by jumping
/// in) can both message it while it works.
fn spawn_subagent(
  parent_id: String,
  prov: provider.Provider,
  api_key: String,
  model: String,
  workspace: String,
  title: String,
  task: String,
) -> String {
  let child_id = wisp.random_string(16)
  subagents.add(parent_id, child_id, title)
  let child =
    session.append(
      session.new(child_id, workspace),
      make_entry(session.User, task, None),
    )
  let _ = session_manager.save(child)
  control.clear(child_id)
  run_store.write(child_id, "running", [], "", 0, [])

  let _ =
    process.spawn_unlinked(fn() {
      case
        engine.run_streaming(
          api_key,
          model,
          workspace,
          engine_config(prov, False, False),
          [],
          task,
          fn(status, steps, context_tokens, net_events) {
            run_store.write(child_id, status, steps, "", context_tokens, net_events)
          },
          fn() { await_decision(child_id, 0) },
          fn() { inbox_of(child_id) },
          subagents_for(child_id, prov, api_key, model, workspace),
          [],
        )
      {
        Ok(outcome) -> {
          // Subagents share the workspace; only top-level turns checkpoint, so
          // the fork tree has one coherent snapshot timeline.
          let _ = session_manager.save(append_turn(child, outcome, None))
          run_store.write(
            child_id,
            "done",
            outcome.steps,
            outcome.text,
            outcome.context_tokens,
            outcome.net_events,
          )
          subagents.set_status(parent_id, child_id, "done")
        }
        Error(message) -> {
          run_store.write(child_id, "error", [], message, 0, [])
          subagents.set_status(parent_id, child_id, "error")
        }
      }
    })
  "Spawned subagent \""
  <> title
  <> "\" with id "
  <> child_id
  <> ". It is running concurrently — `tell` it (target="
  <> child_id
  <> ") to add context, and `collect` it (target="
  <> child_id
  <> ") to wait for and read its result."
}

/// Block until a subagent finishes, then return its result. Capped (~20 min at
/// 250ms/poll) so a stuck child can't pin the parent forever.
fn collect_subagent(child_id: String) -> String {
  collect_loop(child_id, 0)
}

fn collect_loop(child_id: String, polls: Int) -> String {
  case run_store.read_status_text(child_id) {
    Ok(#("done", text)) ->
      "Subagent " <> child_id <> " finished. Result:\n" <> text
    Ok(#("error", text)) -> "Subagent " <> child_id <> " failed: " <> text
    // A live child: keep waiting.
    Ok(#(_running, _)) -> wait_then_collect(child_id, polls)
    // No run for this id at all — a bogus/blank target. Fail fast so the
    // supervisor corrects it instead of blocking on a session that never runs.
    Error(_) ->
      "No subagent with id \""
      <> child_id
      <> "\". Pass the exact id returned by spawn (target=<id>)."
  }
}

fn wait_then_collect(child_id: String, polls: Int) -> String {
  case polls > 4800 {
    True -> "Subagent " <> child_id <> " did not finish within the time limit."
    False -> {
      process.sleep(250)
      collect_loop(child_id, polls + 1)
    }
  }
}

fn worker_port() -> Int {
  case envoy.get("BOUGH_WORKER_PORT") {
    Ok(v) -> int.parse(v) |> result.unwrap(default_worker_port)
    Error(_) -> default_worker_port
  }
}

/// The active branch as `#(role, content)` turns for the agent to replay.
fn history_of(tree: SessionTree) -> List(#(String, String)) {
  session.path(tree)
  |> list.filter_map(fn(e) {
    case e.role {
      session.User -> Ok(#("user", e.content))
      session.Assistant -> Ok(#("assistant", e.content))
      _ -> Error(Nil)
    }
  })
}

// --- Sessions list + fork (resume / branch) ------------------------------

fn list_sessions() -> Response {
  case session_manager.list() {
    Ok(summaries) ->
      json_ok(json.to_string(json.array(summaries, summary_to_json)))
    Error(_) -> wisp.internal_server_error()
  }
}

fn summary_to_json(s: session_manager.Summary) -> json.Json {
  json.object([
    #("id", json.string(s.id)),
    #("project", json.string(s.project)),
    #("title", json.string(s.title)),
    #("turns", json.int(s.turns)),
    #("updated", json.int(s.updated)),
  ])
}

fn fork_session(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, fork_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"entry_id\": string}")
    Ok(entry_id) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) -> {
          let tree = session.set_leaf(tree, entry_id)
          // Restore the filesystem to the forked node's checkpoint, so the
          // working tree matches the branch point, not the latest turn.
          case session.nearest_snapshot(tree, entry_id) {
            Some(ref) -> {
              let _ = snapshots.restore(tree.id, tree.project, ref)
              Nil
            }
            None -> Nil
          }
          case session_manager.save(tree) {
            Ok(_) -> json_ok(json.to_string(session.tree_to_json(tree)))
            Error(_) -> wisp.internal_server_error()
          }
        }
      }
  }
}

fn fork_decoder() -> decode.Decoder(String) {
  use entry_id <- decode.field("entry_id", decode.string)
  decode.success(entry_id)
}

/// Append a completed turn to the tree: each run activity becomes a
/// display-only `ToolResult` entry (content = step JSON, the shape the TUI
/// decodes for the live chat), chained in order, ending in the `Assistant` text
/// entry as the new leaf. `ToolResult` entries are skipped by `history_of`, so
/// the conversation replayed to the model is unchanged. The assistant leaf
/// carries `snapshot_ref` — the filesystem checkpoint for this turn (SPEC §4.1).
fn append_turn(
  tree: SessionTree,
  outcome: agent.Outcome,
  snapshot_ref: Option(String),
) -> SessionTree {
  let tree =
    list.fold(outcome.steps, tree, fn(tr, step) {
      let entry =
        make_entry(
          session.ToolResult,
          agent.step_json_string(step),
          tr.active_leaf,
        )
      session.append(tr, entry)
    })
  let leaf =
    Entry(
      id: wisp.random_string(16),
      parent_id: tree.active_leaf,
      role: session.Assistant,
      content: outcome.text,
      snapshot_ref: snapshot_ref,
      label: None,
      timestamp: clock.now_ms(),
    )
  session.append(tree, leaf)
}

/// Checkpoint the workspace after a top-level turn; `None` if snapshots are
/// disabled or fail (non-fatal — the turn still persists, just without a ref).
fn capture_snapshot(session_id: String, workspace: String) -> Option(String) {
  snapshots.capture(session_id, workspace) |> option.from_result
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

// --- Streaming run (start + poll) ----------------------------------------

fn start_run(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, run_req_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"content\": string}")
    Ok(rr) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) -> launch_run(id, tree, rr.content, rr.review)
      }
  }
}

/// POST `/session/:id/control`: deliver a plan-review (or subagent) decision to
/// the running engine — `{"decision":"allow"}` or
/// `{"decision":"steer","message":...}`.
fn control_run(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, control.request_decoder()) {
    Error(_) ->
      wisp.bad_request("expected {\"decision\":\"allow\"|\"steer\", ...}")
    Ok(decision) -> {
      control.put(id, decision)
      json_ok("{\"status\":\"ok\"}")
    }
  }
}

fn launch_run(
  id: String,
  tree: SessionTree,
  content: String,
  review: Bool,
) -> Response {
  case agent_setup() {
    Error(m) -> json_error(m)
    Ok(#(prov, api_key, model)) -> {
      let history = history_of(tree)
      let user = make_entry(session.User, content, tree.active_leaf)
      let tree = session.append(tree, user)
      let _ = session_manager.save(tree)

      // Drop any stale approval so it can't leak into this fresh run.
      control.clear(id)
      run_store.write(id, "running", [], "", 0, [])
      let _ =
        process.spawn_unlinked(fn() {
          case
            engine.run_streaming(
              api_key,
              model,
              tree.project,
              engine_config(prov, review, net_gate()),
              history,
              content,
              fn(status, steps, context_tokens, net_events) {
                run_store.write(id, status, steps, "", context_tokens, net_events)
              },
              fn() { await_decision(id, 0) },
              fn() { inbox_of(id) },
              subagents_for(id, prov, api_key, model, tree.project),
              tree.allow_domains,
            )
          {
            Ok(outcome) -> {
              // Persist any hosts approved during the run as session state.
              let tree =
                session.SessionTree(..tree, allow_domains: outcome.net_allow)
              let snap = capture_snapshot(id, tree.project)
              let _ = session_manager.save(append_turn(tree, outcome, snap))
              run_store.write(
                id,
                "done",
                outcome.steps,
                outcome.text,
                outcome.context_tokens,
                outcome.net_events,
              )
            }
            Error(message) -> run_store.write(id, "error", [], message, 0, [])
          }
        })
      wisp.json_response("{\"status\":\"started\"}", 202)
    }
  }
}

fn get_run(id: String) -> Response {
  case run_store.read_raw(id) {
    Ok(body) -> json_ok(body)
    Error(_) -> json_ok("{\"status\":\"idle\",\"text\":\"\",\"steps\":[]}")
  }
}

// --- Request bodies ------------------------------------------------------

fn content_decoder() -> decode.Decoder(String) {
  use content <- decode.field("content", decode.string)
  decode.success(content)
}

type RunReq {
  RunReq(content: String, review: Bool)
}

/// Start-run body: the prompt plus an optional `review` flag that turns on the
/// plan-review gate for this run.
fn run_req_decoder() -> decode.Decoder(RunReq) {
  use content <- decode.field("content", decode.string)
  use review <- decode.optional_field("review", False, decode.bool)
  decode.success(RunReq(content: content, review: review))
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
