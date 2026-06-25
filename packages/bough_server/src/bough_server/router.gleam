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
import bough_core/nono
import bough_core/session.{type Entry, type SessionTree, Entry}
import bough_server/agent
import bough_server/clock
import bough_server/control
import bough_server/engine
import bough_server/json_value.{type JsonValue, JArray}
import bough_server/net_profile
import bough_server/packs
import bough_server/nono_bridge
import bough_server/provider
import bough_server/run_store
import bough_server/session_lock
import bough_server/session_manager
import bough_server/skills
import bough_server/snapshots
import bough_server/subagents
import bough_server/workdiff
import bough_server/workfiles
import bough_server/worker_runtime
import envoy
import gleam/dynamic/decode
import gleam/erlang/process
import gleam/http.{Delete, Get, Post}
import gleam/int
import gleam/json
import gleam/list
import gleam/float
import gleam/option.{type Option, None, Some}
import gleam/result
import gleam/string
import simplifile
import wisp.{type Request, type Response}

const default_model = "claude-haiku-4-5-20251001"

const default_openrouter_model = "z-ai/glm-5.2"

const openrouter_base = "https://openrouter.ai/api/v1"

const default_max_turns = 20

const default_worker_port = 8080

// The default worker model: Qwen2.5-Coder-3B, a fast instruct-coder. The
// worker's job is a quick one-shot fix of a failed step, which a fast coder
// does better (and far faster) than a long-chain-of-thought reasoning model;
// experiments put reasoning SLMs (e.g. VibeThinker-3B) far behind here on
// execution latency. Override with BOUGH_WORKER_MODEL (and point
// BOUGH_WORKER_URL / the GGUF at it). This label is sent to the
// OpenAI-compatible endpoint; llama-server serves whatever GGUF it loaded.
fn worker_model() -> String {
  envoy.get("BOUGH_WORKER_MODEL") |> result.unwrap("qwen2.5-coder:3b")
}

pub fn handle_request(req: Request) -> Response {
  let web = web_dir()
  // The web client's static assets (app.js, style.css, logo.svg) live under
  // /static; everything else falls through to the JSON API below. API routes
  // are all under /session, /sessions, /groups, /config, /health, /doc, so
  // nothing is shadowed.
  use <- wisp.serve_static(req, under: "/static", from: web <> "/static")
  case wisp.path_segments(req), req.method {
    [], Get -> serve_index(web)
    ["health"], _ -> json_ok("{\"status\":\"ok\"}")
    ["config"], Get -> config()
    ["groups"], Get -> groups_catalog()
    ["groups", name], Get -> group_detail(name)
    ["skills"], Get -> list_skills()
    ["packs"], Get -> list_packs()
    ["packs"], Post -> save_pack(req)
    ["packs", "draft"], Post -> draft_pack(req)
    ["packs", name], Delete -> delete_pack(name)
    ["doc"], _ -> doc()
    ["sessions"], Get -> list_sessions()
    ["session"], Post -> create_session(req)
    ["session", id], Get -> get_session(id)
    ["session", id, "entry"], Post -> add_entry(req, id)
    ["session", id, "message"], Post -> send_message(req, id)
    ["session", id, "run"], Post -> start_run(req, id)
    ["session", id, "run"], Get -> get_run(req, id)
    ["session", id, "control"], Post -> control_run(req, id)
    ["session", id, "subagents"], Get -> subagents_of(id)
    ["session", id, "diff"], Get -> session_diff(id)
    ["session", id, "files"], Get -> session_files(id)
    ["session", id, "groups"], Get -> get_session_groups(id)
    ["session", id, "groups"], Post -> set_session_groups(req, id)
    ["session", id, "packs"], Post -> apply_packs(req, id)
    ["session", id, "fork"], Post -> fork_session(req, id)
    ["session", id, "graft"], Post -> graft_session(req, id)
    ["session", id, "label"], Post -> label_node(req, id)
    ["session", id, "adopt"], Post -> adopt_branch(req, id)
    _, _ -> wisp.not_found()
  }
}

// --- Web client ----------------------------------------------------------

/// The directory holding the web client's assets: `<priv>/web`. The SPA's
/// `index.html` is served at `/` and its `static/` assets under `/static`.
fn web_dir() -> String {
  case wisp.priv_directory("bough_server") {
    Ok(priv) -> priv <> "/web"
    Error(_) -> "priv/web"
  }
}

/// Serve the single-page web client. Falls back to the service banner if the
/// asset is missing (e.g. running from a build without `priv/web`).
fn serve_index(web: String) -> Response {
  case simplifile.read(web <> "/index.html") {
    Ok(html) -> wisp.html_response(html, 200)
    Error(_) ->
      json_ok(
        "{\"service\":\"bough\",\"version\":\"" <> bough_core.version <> "\"}",
      )
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
        // The network posture so the UI can describe it honestly: leashed
        // (default-deny allowlist) vs fully blocked.
        #("net", json.bool(net_gate())),
      ]),
    ),
  )
}

// --- Capability groups (SPEC §7) -----------------------------------------

/// The nono policy-group catalog for this host (name, description, locked).
fn groups_catalog() -> Response {
  json_ok(json.to_string(json.array(nono_bridge.list_groups(), group_to_json)))
}

fn group_to_json(g: nono.Group) -> json.Json {
  json.object([
    #("name", json.string(g.name)),
    #("description", json.string(g.description)),
    #("platform", json.string(g.platform)),
    #("locked", json.bool(g.locked)),
  ])
}

/// The full contents (granted/denied paths) of one group, for the inspector.
fn group_detail(name: String) -> Response {
  case nono_bridge.group_detail(name) {
    Ok(d) ->
      json_ok(
        json.to_string(
          json.object([
            #("name", json.string(d.name)),
            #("description", json.string(d.description)),
            #(
              "paths",
              json.array(d.paths, fn(p) {
                json.object([
                  #("access", json.string(p.access)),
                  #("path", json.string(p.path)),
                ])
              }),
            ),
          ]),
        ),
      )
    Error(_) -> wisp.not_found()
  }
}

/// A session's enabled (toggleable) groups.
fn get_session_groups(id: String) -> Response {
  case session_manager.load(id) {
    Ok(tree) ->
      json_ok(
        json.to_string(
          json.object([#("groups", json.array(tree.groups, json.string))]),
        ),
      )
    Error(_) -> wisp.not_found()
  }
}

/// Replace a session's enabled groups. Rejects locked/unknown names — only the
/// toggleable catalog entries may be set.
fn set_session_groups(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, groups_req_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"groups\": [string]}")
    Ok(names) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) -> {
          let toggleable =
            nono_bridge.list_groups()
            |> list.filter(fn(g) { !g.locked })
            |> list.map(fn(g) { g.name })
          case list.all(names, fn(n) { list.contains(toggleable, n) }) {
            False -> wisp.bad_request("unknown or locked group")
            True -> {
              // Enabling a group clears it from the advisory suggestions.
              let suggested =
                list.filter(tree.suggested, fn(s) { !list.contains(names, s) })
              let tree =
                session.SessionTree(..tree, groups: names, suggested: suggested)
              case session_manager.save(tree) {
                Ok(_) -> json_ok(json.to_string(session.tree_to_json(tree)))
                Error(_) -> wisp.internal_server_error()
              }
            }
          }
        }
      }
  }
}

fn groups_req_decoder() -> decode.Decoder(List(String)) {
  use groups <- decode.field("groups", decode.list(decode.string))
  decode.success(groups)
}

// --- Allowlist packs -----------------------------------------------------

fn list_packs() -> Response {
  json_ok(json.to_string(json.array(packs.list(), packs.to_json)))
}

/// GET `/skills`: the installed skills (name + description) under ~/.bough/skills.
fn list_skills() -> Response {
  json_ok(json.to_string(skills.to_json(skills.list())))
}

/// Upsert a pack (by name).
fn save_pack(req: Request) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, packs.decoder()) {
    Error(_) ->
      wisp.bad_request(
        "expected {\"name\":string, \"description\":string, \"groups\":[string], \"allow\":[string]}",
      )
    Ok(pack) ->
      case string.trim(pack.name) {
        "" -> wisp.bad_request("pack name is required")
        _ -> {
          packs.save(pack)
          json_ok(json.to_string(packs.to_json(pack)))
        }
      }
  }
}

fn delete_pack(name: String) -> Response {
  packs.delete(name)
  json_ok("{\"status\":\"ok\"}")
}

/// Apply named packs to a session: union their groups + allow-rules into the
/// session (the profile recomposes from those). Unknown/locked groups dropped.
fn apply_packs(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, names_req_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"names\": [string]}")
    Ok(names) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) -> {
          let chosen = list.filter_map(names, packs.get)
          let toggleable =
            nono_bridge.list_groups()
            |> list.filter(fn(g) { !g.locked })
            |> list.map(fn(g) { g.name })
          let groups =
            list.fold(chosen, tree.groups, fn(acc, p) {
              list.append(acc, p.groups)
            })
            |> list.unique
            |> list.filter(fn(g) { list.contains(toggleable, g) })
          let allow =
            list.fold(chosen, tree.allow_domains, fn(acc, p) {
              list.append(acc, p.allow)
            })
            |> list.unique
          let suggested =
            list.filter(tree.suggested, fn(s) { !list.contains(groups, s) })
          let tree =
            session.SessionTree(
              ..tree,
              groups: groups,
              allow_domains: allow,
              suggested: suggested,
            )
          case session_manager.save(tree) {
            Ok(_) -> json_ok(json.to_string(session.tree_to_json(tree)))
            Error(_) -> wisp.internal_server_error()
          }
        }
      }
  }
}

fn names_req_decoder() -> decode.Decoder(List(String)) {
  use names <- decode.field("names", decode.list(decode.string))
  decode.success(names)
}

/// Draft a pack from a natural-language description: a one-shot supervisor-model
/// call returns the minimal network allow-rules + capability groups the work
/// needs. Returns a *draft* (not saved) for the human to review and edit.
fn draft_pack(req: Request) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, draft_req_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"description\": string}")
    Ok(description) ->
      case agent_setup() {
        Error(m) -> json_error(m)
        Ok(#(prov, key, model)) -> {
          let catalog =
            nono_bridge.list_groups() |> list.filter(fn(g) { !g.locked })
          let listing =
            catalog
            |> list.map(fn(g) { "- " <> g.name <> ": " <> g.description })
            |> string.join("\n")
          let user =
            "Work to be sandboxed:\n"
            <> description
            <> "\n\nAvailable capability groups (choose only from these exact names; pick none if filesystem access beyond the workspace isn't needed):\n"
            <> listing
          case
            provider.complete(
              prov,
              key,
              model,
              pack_draft_system,
              [provider.user_text(user)],
              "propose_pack",
              "Propose the minimal sandbox allowlist for the described work.",
              pack_schema(),
            )
          {
            Error(e) -> json_error(e)
            Ok(resp) ->
              case resp.tool_uses {
                [tu, ..] -> {
                  let names = list.map(catalog, fn(g) { g.name })
                  let groups =
                    string_list(tu.input, "groups")
                    |> list.filter(fn(g) { list.contains(names, g) })
                  let allow = string_list(tu.input, "allow")
                  json_ok(
                    json.to_string(
                      json.object([
                        #("groups", json.array(groups, json.string)),
                        #("allow", json.array(allow, json.string)),
                      ]),
                    ),
                  )
                }
                [] -> json_error("the model did not propose a pack")
              }
          }
        }
      }
  }
}

fn draft_req_decoder() -> decode.Decoder(String) {
  use description <- decode.field("description", decode.string)
  decode.success(description)
}

/// Pull a list of strings from a JsonValue object field (the model's tool args).
fn string_list(input: JsonValue, key: String) -> List(String) {
  case json_value.field(input, key) {
    Ok(JArray(items)) -> list.filter_map(items, json_value.as_string)
    _ -> []
  }
}

const pack_draft_system = "You design a minimal, least-privilege sandbox allowlist for a coding agent. Given a description of the work, return: `allow` — the network hosts (e.g. \"api.github.com\") or METHOD-path globs (e.g. \"https://api.foo.com/v1/**\") the work legitimately needs, and `groups` — capability group names drawn ONLY from the provided catalog. Be conservative: include only what the described work plainly requires, prefer specific hosts over broad ones, and return empty lists rather than guessing. Do not invent group names."

fn pack_schema() -> json.Json {
  let str_array = fn(desc) {
    json.object([
      #("type", json.string("array")),
      #("items", json.object([#("type", json.string("string"))])),
      #("description", json.string(desc)),
    ])
  }
  json.object([
    #("type", json.string("object")),
    #(
      "properties",
      json.object([
        #(
          "allow",
          str_array(
            "network hosts or METHOD path-globs to allowlist for the work",
          ),
        ),
        #(
          "groups",
          str_array("capability group names from the provided catalog"),
        ),
      ]),
    ),
    #(
      "required",
      json.preprocessed_array([json.string("allow"), json.string("groups")]),
    ),
  ])
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
      grafted_from: None,
    )
  // Append under the session lock so concurrent appends (e.g. parallel branch
  // runs) serialize against the freshest tree instead of clobbering each other.
  case session_lock.mutate(tree.id, fn(fresh) { session.append(fresh, entry) }) {
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
          tree.groups,
        )
      {
        Error(message) -> {
          let _ = session_manager.save(tree)
          json_error(message)
        }
        Ok(outcome) -> {
          let snap = capture_snapshot(tree.id, tree.project)
          let #(tree, leaf) = append_turn(tree, outcome, snap, tree.active_leaf)
          let tree = session.set_leaf(tree, leaf)
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

/// Engine config from the environment. The worker (VibeThinker-3B, SPEC.md §5.6)
/// is always enabled: bough ensures a local llama-server is up and points the
/// worker at it. Set `BOUGH_WORKER_URL` to use a remote endpoint instead
/// (honored inside `worker_runtime.ensure`).
fn engine_config(
  prov: provider.Provider,
  review: Bool,
  net_gate: Bool,
) -> engine.Config {
  let base = engine.default_config()
  // If the worker server can't be brought up, disable the worker outright
  // rather than pointing every fix attempt at a dead URL — otherwise a failed
  // step floods the run with "worker unavailable" notices. The supervisor then
  // does its own fixes (engine gates the whole path on `worker`).
  let #(worker, worker_url) = case worker_runtime.ensure(worker_port()) {
    Ok(url) -> #(Some(worker_model()), url)
    Error(reason) -> {
      wisp.log_warning("worker disabled: " <> reason)
      #(None, base.worker_url)
    }
  }
  let #(worker_temp, worker_top_p) = worker_decoding(base)
  engine.Config(
    ..base,
    provider: prov,
    worker: worker,
    worker_url: worker_url,
    max_rounds: max_rounds(),
    review: review,
    net_gate: net_gate,
    net_credentials: net_credentials(),
    worker_temperature: worker_temp,
    worker_top_p: worker_top_p,
  )
}

/// Worker decoding from the environment, falling back to the defaults (suited to
/// the fast-coder default worker). Set BOUGH_WORKER_TEMP / BOUGH_WORKER_TOP_P
/// when swapping in a reasoning worker (e.g. VibeThinker-3B wants 1.0 / 0.95).
fn worker_decoding(base: engine.Config) -> #(Option(Float), Option(Float)) {
  let parse = fn(name, fallback) {
    case envoy.get(name) {
      Ok(v) ->
        case float.parse(v) {
          Ok(f) -> Some(f)
          Error(_) -> fallback
        }
      Error(_) -> fallback
    }
  }
  #(
    parse("BOUGH_WORKER_TEMP", base.worker_temperature),
    parse("BOUGH_WORKER_TOP_P", base.worker_top_p),
  )
}

/// The network sandbox is always-on: every run gets default-deny egress with
/// per-host approval — nothing leaves unless it's on the allowlist. `BOUGH_NET=0`
/// is the escape hatch that fully blocks the network instead (no prompts).
fn net_gate() -> Bool {
  envoy.get("BOUGH_NET") != Ok("0")
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
/// OpenRouter / z-ai/glm-5.2; set `BOUGH_PROVIDER=anthropic` for
/// Anthropic / claude-haiku-4-5.
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

/// The agent's uncommitted changes in the session's workspace (SPEC §4.1) — a
/// review surface so a human can see what was written before keeping it.
fn session_diff(id: String) -> Response {
  case session_manager.load(id) {
    Error(_) -> wisp.not_found()
    Ok(tree) -> {
      let #(is_git, files, patch) = workdiff.working_diff(tree.project)
      json_ok(
        json.to_string(
          json.object([
            #("git", json.bool(is_git)),
            #("patch", json.string(patch)),
            #(
              "files",
              json.array(files, fn(f) {
                json.object([
                  #("status", json.string(f.status)),
                  #("path", json.string(f.path)),
                ])
              }),
            ),
          ]),
        ),
      )
    }
  }
}

/// GET `/session/:id/files`: workspace-relative file paths for the composer's
/// "@" file picker (git-tracked + untracked-not-ignored, or a `find` fallback).
fn session_files(id: String) -> Response {
  case session_manager.load(id) {
    Error(_) -> wisp.not_found()
    Ok(tree) ->
      json_ok(
        json.to_string(
          json.object([
            #(
              "files",
              json.array(workfiles.list_files(tree.project), json.string),
            ),
          ]),
        ),
      )
  }
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
    spawn: fn(title, task, wake) {
      spawn_subagent(parent_id, prov, api_key, model, workspace, title, task, wake)
    },
    tell: fn(target, message) {
      control.put(target, control.Steer(message))
      "Message queued for subagent " <> target <> "."
    },
    collect: fn(target) { collect_subagent(target) },
    pending: fn() {
      list.any(subagents.list(parent_id), fn(s) { s.status == "running" })
    },
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
  wake: process.Subject(Nil),
) -> String {
  let child_id = wisp.random_string(16)
  subagents.add(parent_id, child_id, title)
  // Subagents share the workspace, so they inherit the parent's enabled groups.
  let inherited = case session_manager.load(parent_id) {
    Ok(parent) -> parent.groups
    Error(_) -> []
  }
  let child =
    session.append(
      session.SessionTree(..session.new(child_id, workspace), groups: inherited),
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
          fn() { control.stop_requested(child_id) },
          subagents_for(child_id, prov, api_key, model, workspace),
          [],
          inherited,
          [],
        )
      {
        Ok(outcome) -> {
          // Subagents share the workspace; only top-level turns checkpoint, so
          // the fork tree has one coherent snapshot timeline. Persist any groups
          // the subagent asked for / enabled so its requests show when you view it.
          let child =
            session.SessionTree(
              ..child,
              groups: list.unique(list.append(child.groups, outcome.groups)),
              suggested: outcome.suggested,
            )
          // A subagent is its own linear session, so follow the view onto each
          // turn it completes.
          let #(child, leaf) =
            append_turn(child, outcome, None, child.active_leaf)
          let _ = session_manager.save(session.set_leaf(child, leaf))
          run_store.write(
            child_id,
            "done",
            outcome.steps,
            outcome.text,
            outcome.context_tokens,
            outcome.net_events,
          )
          // Push the final output to the parent's inbox BEFORE flipping the
          // status, so a parent waiting on `pending` never sees the child go
          // idle without its result already queued.
          control.put(
            parent_id,
            control.Steer(
              "Subagent \""
              <> title
              <> "\" ("
              <> child_id
              <> ") finished. Final output:\n"
              <> outcome.text,
            ),
          )
          subagents.set_status(parent_id, child_id, "done")
          // Wake the parent run process now that its result is queued — instant,
          // no poll. (Harmless if the parent isn't currently waiting; the message
          // just buffers in its mailbox.)
          process.send(wake, Nil)
        }
        Error(message) -> {
          run_store.write(child_id, "error", [], message, 0, [])
          control.put(
            parent_id,
            control.Steer(
              "Subagent \""
              <> title
              <> "\" ("
              <> child_id
              <> ") failed: "
              <> message,
            ),
          )
          subagents.set_status(parent_id, child_id, "error")
          process.send(wake, Nil)
        }
      }
    })
  "Spawned subagent \""
  <> title
  <> "\" with id "
  <> child_id
  <> ". It runs concurrently and its final output is delivered to you "
  <> "automatically when it finishes — you do NOT need to block on it. `tell` "
  <> "it (target="
  <> child_id
  <> ") to add context, or `collect` it (target="
  <> child_id
  <> ") to check its current status without waiting."
}

/// Report a subagent's current status without blocking. A finished child's
/// final output is pushed to the parent's inbox automatically (see
/// `spawn_subagent`), so the parent never sits and waits — `collect` is just a
/// non-blocking status probe.
fn collect_subagent(child_id: String) -> String {
  case run_store.read_status_text(child_id) {
    Ok(#("done", _)) ->
      "Subagent "
      <> child_id
      <> " has finished; its final output has been delivered to you as a "
      <> "message."
    Ok(#("error", text)) -> "Subagent " <> child_id <> " failed: " <> text
    // Still running: don't block. Its result will arrive on its own.
    Ok(#(_running, _)) ->
      "Subagent "
      <> child_id
      <> " is still running. You don't need to wait — its final output will be "
      <> "delivered to you automatically when it finishes. Carry on with other "
      <> "work, or `tell` it (target="
      <> child_id
      <> ") if it needs steering."
    // No run for this id at all — a bogus/blank target.
    Error(_) ->
      "No subagent with id \""
      <> child_id
      <> "\". Pass the exact id returned by spawn (target=<id>)."
  }
}

fn worker_port() -> Int {
  case envoy.get("BOUGH_WORKER_PORT") {
    Ok(v) -> int.parse(v) |> result.unwrap(default_worker_port)
    Error(_) -> default_worker_port
  }
}

/// The active branch as `#(role, content)` turns for the agent to replay. A
/// `System` action digest (written by `append_turn`) is folded into the front of
/// the assistant turn it precedes, so the supervisor sees what it did last turn
/// rather than only its own prose. `ToolResult` display entries are skipped.
pub fn history_of(tree: SessionTree) -> List(#(String, String)) {
  let #(turns, pending) =
    session.path(tree)
    |> list.fold(#([], None), fn(acc, e) {
      let #(turns, pending) = acc
      case e.role {
        session.User -> #([#("user", e.content), ..turns], pending)
        session.System -> #(turns, Some(e.content))
        session.Assistant -> {
          let content = case pending {
            Some(digest) -> digest <> "\n\n" <> e.content
            None -> e.content
          }
          #([#("assistant", content), ..turns], None)
        }
        session.ToolResult -> #(turns, pending)
      }
    })
  // A trailing digest with no assistant turn after it (e.g. the branch was
  // forked onto a System node) is still replayed so the work isn't lost.
  let turns = case pending {
    Some(digest) -> [#("assistant", digest), ..turns]
    None -> turns
  }
  list.reverse(turns)
}

/// A typed digest of the actions a turn performed — verbs, targets, and exit
/// codes, but not their output (dropped on purpose to keep replayed context
/// small). `None` for a purely conversational turn, so chat-only turns stay
/// clean. Derived from the typed steps, so no display-JSON re-parsing.
pub fn actions_summary(steps: List(agent.Step)) -> Option(String) {
  let #(lines, _) =
    list.fold(steps, #([], ""), fn(acc, step) {
      let #(lines, arg) = acc
      case step {
        // A call's arg (command/path/pattern) is paired with the exec that
        // follows it; hold it until then.
        agent.StepCall(_verb, a, _detail) -> #(lines, a)
        agent.StepExec(verb, exit, _digest) -> {
          let target = case string.trim(arg) {
            "" -> ""
            a -> " " <> oneline_clip(a, 60)
          }
          let line = verb <> target <> " (exit " <> int.to_string(exit) <> ")"
          #([line, ..lines], "")
        }
        agent.StepWorker(_brief, _command, exit) -> #(
          ["worker fix (exit " <> int.to_string(exit) <> ")", ..lines],
          arg,
        )
        agent.StepCheck(ok, _digest) -> {
          let verdict = case ok {
            True -> "passed"
            False -> "failed"
          }
          #(["CHECK " <> verdict, ..lines], arg)
        }
        // Prose, review notes, and gate events aren't actions.
        _ -> #(lines, arg)
      }
    })
  case list.reverse(lines) {
    [] -> None
    ls ->
      Some(
        "[Context — actions you performed this turn (output omitted): "
        <> string.join(ls, "; ")
        <> "]",
      )
  }
}

/// Collapse to one line and clip to `max` chars with an ellipsis if longer.
fn oneline_clip(s: String, max: Int) -> String {
  let one = s |> string.replace("\n", " ") |> string.trim
  case string.length(one) > max {
    True -> string.slice(one, 0, max) <> "…"
    False -> one
  }
}

// --- Sessions list + fork (resume / branch) ------------------------------

fn list_sessions() -> Response {
  case session_manager.list() {
    Ok(summaries) -> {
      // Spawned subagents are sessions too; keep them out of the top-level list
      // (they live under their parent's Subagents pane) so delegating to a team
      // doesn't flood the sidebar.
      let children = subagents.child_ids()
      let top = list.filter(summaries, fn(s) { !list.contains(children, s.id) })
      json_ok(json.to_string(json.array(top, summary_to_json)))
    }
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
          // Switching what you *view* no longer rewrites the working tree — the
          // project dir tracks `trunk_leaf`, not the viewed branch. Use `adopt`
          // to bring a branch's files into trunk on purpose.
          let tree = session.set_leaf(tree, entry_id)
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

/// Adopt a branch as trunk: restore the project working dir to its snapshot and
/// move the trunk pointer (and the view) to it. The explicit, opt-in version of
/// what `fork` used to do silently on every switch.
fn adopt_branch(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, fork_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"entry_id\": string}")
    Ok(leaf) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) -> {
          case session.nearest_snapshot(tree, leaf) {
            Some(ref) -> {
              let _ = snapshots.restore(tree.id, tree.project, ref)
              Nil
            }
            None -> Nil
          }
          let tree = session.set_leaf(session.set_trunk(tree, leaf), leaf)
          case session_manager.save(tree) {
            Ok(_) -> json_ok(json.to_string(session.tree_to_json(tree)))
            Error(_) -> wisp.internal_server_error()
          }
        }
      }
  }
}

/// Name a node (a branch by its tip). Pure metadata — no filesystem touch.
fn label_node(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, label_decoder()) {
    Error(_) -> wisp.bad_request("expected {\"entry_id\": string, \"label\": string}")
    Ok(#(entry_id, label)) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) -> {
          let tree = session.set_label(tree, entry_id, label)
          case session_manager.save(tree) {
            Ok(_) -> json_ok(json.to_string(session.tree_to_json(tree)))
            Error(_) -> wisp.internal_server_error()
          }
        }
      }
  }
}

fn label_decoder() -> decode.Decoder(#(String, String)) {
  use entry_id <- decode.field("entry_id", decode.string)
  use label <- decode.field("label", decode.string)
  decode.success(#(entry_id, label))
}

/// Reattach the subtree rooted at `section_root` onto `onto` (SPEC.md §4.2). A
/// graft moves the conversation only — the copies carry no snapshot — so there
/// is no filesystem restore here, unlike `fork`. The working tree stays as it is
/// until the next run rebuilds against it.
fn graft_session(req: Request, id: String) -> Response {
  use body <- wisp.require_json(req)
  case decode.run(body, graft_decoder()) {
    Error(_) ->
      wisp.bad_request("expected {\"section_root\": string, \"onto\": string}")
    Ok(#(section_root, onto)) ->
      case session_manager.load(id) {
        Error(_) -> wisp.not_found()
        Ok(tree) ->
          case
            session.plan_graft(
              tree,
              section_root,
              onto,
              wisp.random_string(16),
              clock.now_ms(),
            )
          {
            Error(_) ->
              wisp.bad_request(
                "invalid graft: unknown node, or onto is inside the section",
              )
            Ok(graft) -> {
              let tree = session.apply_graft(tree, graft)
              case session_manager.save(tree) {
                Ok(_) -> json_ok(json.to_string(session.tree_to_json(tree)))
                Error(_) -> wisp.internal_server_error()
              }
            }
          }
      }
  }
}

fn graft_decoder() -> decode.Decoder(#(String, String)) {
  use section_root <- decode.field("section_root", decode.string)
  use onto <- decode.field("onto", decode.string)
  decode.success(#(section_root, onto))
}

/// Append a completed turn to the tree: each run activity becomes a
/// display-only `ToolResult` entry (content = step JSON, the shape the TUI
/// decodes for the live chat), chained in order, ending in the `Assistant` text
/// entry as the new leaf. `ToolResult` entries are skipped by `history_of`, so
/// the conversation replayed to the model is unchanged. The assistant leaf
/// carries `snapshot_ref` — the filesystem checkpoint for this turn (SPEC §4.1).
///
/// When the turn ran actions, a compact `System` entry holding a typed digest of
/// them (verbs, targets, exit codes) is chained just before the assistant leaf.
/// The supervisor's real tool_use/tool_result blocks don't survive the run, so
/// without this the next turn can't see what it did (it would only replay its
/// own prose); `history_of` folds the digest back into that turn's context.
/// Append a completed turn's entries onto `anchor` (the run's own user entry),
/// not the live `active_leaf` — so a concurrent branch run lands its turn on
/// its own branch even if the human has navigated elsewhere. Adds without
/// moving the view; returns the tree and the new assistant leaf id so the
/// caller can advance trunk / follow the view as appropriate.
fn append_turn(
  tree: SessionTree,
  outcome: agent.Outcome,
  snapshot_ref: Option(String),
  anchor: Option(String),
) -> #(SessionTree, String) {
  let #(tree, parent) =
    list.fold(outcome.steps, #(tree, anchor), fn(acc, step) {
      let #(tr, p) = acc
      let entry = make_entry(session.ToolResult, agent.step_json_string(step), p)
      #(session.add(tr, entry), Some(entry.id))
    })
  let #(tree, parent) = case actions_summary(outcome.steps) {
    Some(summary) -> {
      let entry = make_entry(session.System, summary, parent)
      #(session.add(tree, entry), Some(entry.id))
    }
    None -> #(tree, parent)
  }
  let leaf =
    Entry(
      id: wisp.random_string(16),
      parent_id: parent,
      role: session.Assistant,
      content: outcome.text,
      snapshot_ref: snapshot_ref,
      label: None,
      timestamp: clock.now_ms(),
      grafted_from: None,
    )
  #(session.add(tree, leaf), leaf.id)
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
    grafted_from: None,
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
  // A stop is a separate channel from the decision queue (so it lands no matter
  // what is pending and never collides with a steer message).
  case decode.run(body, decision_kind_decoder()) {
    Ok("stop") -> {
      control.request_stop(id)
      json_ok("{\"status\":\"ok\"}")
    }
    _ ->
      case decode.run(body, control.request_decoder()) {
        Error(_) ->
          wisp.bad_request(
            "expected {\"decision\":\"allow\"|\"steer\"|\"stop\", ...}",
          )
        Ok(decision) -> {
          control.put(id, decision)
          json_ok("{\"status\":\"ok\"}")
        }
      }
  }
}

fn decision_kind_decoder() -> decode.Decoder(String) {
  use kind <- decode.field("decision", decode.string)
  decode.success(kind)
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
      // A run on the trunk branch acts on the real project dir and advances
      // trunk; a run on any other branch acts on an isolated worktree
      // materialized from that branch's snapshot, leaving trunk untouched.
      let on_trunk = tree.active_leaf == tree.trunk_leaf
      let branch_leaf = tree.active_leaf
      let history = history_of(tree)
      // The run's own anchor: a fresh user entry. Keying run state (and the
      // worktree) by it — not the branch leaf — gives each run its own status
      // slot and its own scratch dir, so two runs off the same branch don't
      // collide.
      let user = make_entry(session.User, content, branch_leaf)
      let run_key = user.id
      case run_workspace(tree, on_trunk, run_key, branch_leaf) {
        Error(m) -> json_error(m)
        Ok(workspace) -> {
      // Append the user turn under the lock so a concurrent branch run can't
      // clobber it; follow the view onto it because the human is looking here.
      let _ =
        session_lock.mutate(id, fn(fresh) {
          let fresh = session.add(fresh, user)
          case fresh.active_leaf == branch_leaf {
            True -> session.set_leaf(fresh, run_key)
            False -> fresh
          }
        })

      // Drop any stale approval so it can't leak into this fresh run.
      control.clear(id)
      run_store.write(run_key, "running", [], "", 0, [])
      let _ =
        process.spawn_unlinked(fn() {
          case
            engine.run_streaming(
              api_key,
              model,
              workspace,
              engine_config(prov, review, net_gate()),
              history,
              content,
              fn(status, steps, context_tokens, net_events) {
                run_store.write(run_key, status, steps, "", context_tokens, net_events)
              },
              fn() { await_decision(id, 0) },
              fn() { inbox_of(id) },
              fn() { control.stop_requested(id) },
              subagents_for(id, prov, api_key, model, workspace),
              tree.allow_domains,
              tree.groups,
              tree.suggested,
            )
          {
            Ok(outcome) -> {
              // Capture from the dir the run actually used: the project dir on
              // trunk, the branch worktree otherwise (committed through the
              // worktree's own HEAD so it never collides with trunk).
              let snap = case on_trunk {
                True -> capture_snapshot(id, workspace)
                False ->
                  snapshots.capture_worktree(workspace) |> option.from_result
              }
              // Fold the turn into the freshest tree under the lock: build it on
              // this run's anchor (not the live active_leaf, which the human may
              // have moved), union the groups/allowlist the run earned, advance
              // trunk only if this was the trunk run and trunk hasn't moved, and
              // follow the view only if it's still parked on this branch's tip.
              let _ =
                session_lock.mutate(id, fn(fresh) {
                  let groups =
                    list.unique(list.append(fresh.groups, outcome.groups))
                  let suggested =
                    list.filter(outcome.suggested, fn(s) {
                      !list.contains(groups, s)
                    })
                  let fresh =
                    session.SessionTree(
                      ..fresh,
                      groups: groups,
                      allow_domains: outcome.net_allow,
                      suggested: suggested,
                    )
                  let #(fresh, newleaf) =
                    append_turn(fresh, outcome, snap, Some(run_key))
                  let fresh = case on_trunk && fresh.trunk_leaf == branch_leaf {
                    True -> session.set_trunk(fresh, newleaf)
                    False -> fresh
                  }
                  case fresh.active_leaf == Some(run_key) {
                    True -> session.set_leaf(fresh, newleaf)
                    False -> fresh
                  }
                })
              case on_trunk {
                False -> snapshots.remove_worktree(id, run_key)
                True -> Nil
              }
              run_store.write(
                run_key,
                "done",
                outcome.steps,
                outcome.text,
                outcome.context_tokens,
                outcome.net_events,
              )
            }
            Error(message) ->
              run_store.write(run_key, "error", [], message, 0, [])
          }
        })
      wisp.json_response("{\"status\":\"started\"}", 202)
        }
      }
    }
  }
}

/// Pick the working directory for a run: the real project dir on trunk, or a
/// fresh worktree (keyed by the run's anchor, so concurrent runs off one branch
/// don't share a dir) materialized from the branch's snapshot.
fn run_workspace(
  tree: SessionTree,
  on_trunk: Bool,
  run_key: String,
  branch_leaf: Option(String),
) -> Result(String, String) {
  case on_trunk {
    True -> Ok(tree.project)
    False ->
      case branch_leaf {
        None -> Error("No branch selected to run.")
        Some(leaf) ->
          case session.nearest_snapshot(tree, leaf) {
            Some(ref) -> snapshots.materialize_worktree(tree.id, run_key, ref)
            None ->
              Error(
                "This branch has no snapshot to run from. Adopt it to trunk first, then run.",
              )
          }
      }
  }
}

/// Run progress. Top-level runs are keyed by their anchor (the branch's pending
/// user-tip), so by default we resolve the viewed branch's run via active_leaf;
/// `?key=<leaf>` polls a specific branch. Falls back to the session id, which
/// is how subagent runs are keyed.
fn get_run(req: Request, id: String) -> Response {
  let resolved = case wisp.get_query(req) |> list.key_find("key") {
    Ok(k) -> k
    Error(_) ->
      case session_manager.load(id) {
        Ok(tree) -> option.unwrap(tree.active_leaf, id)
        Error(_) -> id
      }
  }
  case run_store.read_raw(resolved) {
    Ok(body) -> json_ok(body)
    Error(_) ->
      case run_store.read_raw(id) {
        Ok(body) -> json_ok(body)
        Error(_) -> json_ok("{\"status\":\"idle\",\"text\":\"\",\"steps\":[]}")
      }
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
