//// Inbound control to a running engine (SPEC §8): the bridge that lets a client
//// talk back to a run that is already executing. It is disk-based to match the
//// polling `run_store` — the HTTP handler writes a decision, the engine process
//// polls for it. Two consumers:
////
////   - the plan-review gate: a pending plan is `Allow`ed or `Steer`ed (rejected
////     with guidance) before the harness executes it;
////   - subagents: a `Steer` message the human adds to a child run, picked up at
////     the child's next round.
////
//// One slot per session id; `take` is read-once (it deletes the file), so a
//// decision is consumed exactly once.

import envoy
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/result
import simplifile

pub type Decision {
  /// Approve the pending plan as-is (or, for a subagent, "carry on").
  Allow
  /// Reject / redirect with a human message fed back to the supervisor.
  Steer(message: String)
}

fn dir() -> Result(String, Nil) {
  use home <- result.try(envoy.get("HOME"))
  let d = home <> "/.bough/control"
  let _ = simplifile.create_directory_all(d)
  Ok(d)
}

fn path(d: String, session_id: String) -> String {
  d <> "/" <> session_id <> ".json"
}

/// Enqueue a decision for a session (FIFO). Several messages may be in flight —
/// e.g. the parent agent sending a running subagent successive context — so this
/// appends rather than replacing; `take` consumes them oldest-first.
pub fn put(session_id: String, decision: Decision) -> Nil {
  case dir() {
    Error(_) -> Nil
    Ok(d) -> write_queue(d, session_id, list.append(read_queue(d, session_id), [decision]))
  }
}

/// Consume the oldest pending decision for a session, if any (read-once).
pub fn take(session_id: String) -> Result(Decision, Nil) {
  use d <- result.try(dir())
  case read_queue(d, session_id) {
    [] -> Error(Nil)
    [head, ..rest] -> {
      write_queue(d, session_id, rest)
      Ok(head)
    }
  }
}

fn read_queue(d: String, session_id: String) -> List(Decision) {
  case simplifile.read(path(d, session_id)) {
    Error(_) -> []
    Ok(body) -> json.parse(body, decode.list(decoder())) |> result.unwrap([])
  }
}

fn write_queue(d: String, session_id: String, queue: List(Decision)) -> Nil {
  let target = path(d, session_id)
  case queue {
    [] -> {
      let _ = simplifile.delete(target)
      Nil
    }
    _ -> {
      let tmp = target <> ".tmp"
      let body = json.to_string(json.array(queue, to_json))
      case simplifile.write(tmp, body) {
        Ok(_) -> {
          let _ = simplifile.rename(tmp, target)
          Nil
        }
        Error(_) -> Nil
      }
    }
  }
}

/// Drop any pending decision for a session (e.g. when a fresh run starts) so a
/// stale approval can't leak into it.
pub fn clear(session_id: String) -> Nil {
  case dir() {
    Error(_) -> Nil
    Ok(d) -> {
      let _ = simplifile.delete(path(d, session_id))
      Nil
    }
  }
}

fn to_json(decision: Decision) -> json.Json {
  case decision {
    Allow -> json.object([#("decision", json.string("allow"))])
    Steer(message) ->
      json.object([
        #("decision", json.string("steer")),
        #("message", json.string(message)),
      ])
  }
}

fn decoder() -> decode.Decoder(Decision) {
  use kind <- decode.field("decision", decode.string)
  case kind {
    "steer" -> {
      use message <- decode.optional_field("message", "", decode.string)
      decode.success(Steer(message))
    }
    _ -> decode.success(Allow)
  }
}

/// Decode a `{"decision":..., "message":...}` request body (for the HTTP route).
pub fn request_decoder() -> decode.Decoder(Decision) {
  decoder()
}
