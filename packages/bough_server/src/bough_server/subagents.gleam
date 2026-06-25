//// Registry of subagents a session has spawned (SPEC §5). Each parent session
//// owns a small JSON list of its children — `{id, title, status}` — written by
//// the spawn orchestration and read by `GET /session/:id/subagents` so a client
//// can list them and jump into one. The child's transcript and live progress
//// are just its own session + run, addressed by the child id.
////
//// Writes for one parent are sequential (a parent's spawn steps run in order),
//// so a plain read-modify-write file is enough.

import envoy
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/result
import gleam/string
import simplifile

pub type Sub {
  Sub(id: String, title: String, status: String)
}

fn dir() -> Result(String, Nil) {
  use home <- result.try(envoy.get("HOME"))
  let d = home <> "/.bough/subagents"
  let _ = simplifile.create_directory_all(d)
  Ok(d)
}

fn path(d: String, parent_id: String) -> String {
  d <> "/" <> parent_id <> ".json"
}

/// The children registered under a parent, oldest first.
pub fn list(parent_id: String) -> List(Sub) {
  case dir() {
    Error(_) -> []
    Ok(d) ->
      case simplifile.read(path(d, parent_id)) {
        Error(_) -> []
        Ok(body) ->
          json.parse(body, decode.list(sub_decoder())) |> result.unwrap([])
      }
  }
}

/// Every session id registered as a child of some parent. Used to keep spawned
/// subagents out of the top-level session list — they're reachable through
/// their parent's Subagents pane, not the main sidebar.
pub fn child_ids() -> List(String) {
  case dir() {
    Error(_) -> []
    Ok(d) ->
      case simplifile.read_directory(d) {
        Error(_) -> []
        Ok(files) ->
          files
          |> list.filter(fn(f) { string.ends_with(f, ".json") })
          |> list.flat_map(fn(f) {
            list.map(list(string.drop_end(f, 5)), fn(s) { s.id })
          })
      }
  }
}

/// Record a freshly spawned child (status "running").
pub fn add(parent_id: String, id: String, title: String) -> Nil {
  let existing = list(parent_id)
  save(parent_id, list.append(existing, [Sub(id, title, "running")]))
}

/// Update a child's status ("done" / "error").
pub fn set_status(parent_id: String, id: String, status: String) -> Nil {
  let updated =
    list.map(list(parent_id), fn(s) {
      case s.id == id {
        True -> Sub(..s, status: status)
        False -> s
      }
    })
  save(parent_id, updated)
}

fn save(parent_id: String, subs: List(Sub)) -> Nil {
  case dir() {
    Error(_) -> Nil
    Ok(d) -> {
      let target = path(d, parent_id)
      let tmp = target <> ".tmp"
      case simplifile.write(tmp, json.to_string(to_json(subs))) {
        Ok(_) -> {
          let _ = simplifile.rename(tmp, target)
          Nil
        }
        Error(_) -> Nil
      }
    }
  }
}

pub fn to_json(subs: List(Sub)) -> json.Json {
  json.array(subs, fn(s) {
    json.object([
      #("id", json.string(s.id)),
      #("title", json.string(s.title)),
      #("status", json.string(s.status)),
    ])
  })
}

fn sub_decoder() -> decode.Decoder(Sub) {
  use id <- decode.field("id", decode.string)
  use title <- decode.field("title", decode.string)
  use status <- decode.field("status", decode.string)
  decode.success(Sub(id:, title:, status:))
}
