//// Per-session run progress, written by the background agent process and read
//// by the polling `GET /session/:id/run` endpoint. The single writer rewrites
//// the file atomically (temp + rename) after each step; readers tolerate a
//// missing file (idle).

import bough_core/nono.{type AuditEvent}
import bough_server/agent.{type Step}
import envoy
import gleam/dynamic/decode
import gleam/json
import gleam/result
import simplifile

fn dir() -> Result(String, Nil) {
  use home <- result.try(envoy.get("HOME"))
  let d = home <> "/.bough/runs"
  let _ = simplifile.create_directory_all(d)
  Ok(d)
}

fn path(d: String, session_id: String) -> String {
  d <> "/" <> session_id <> ".json"
}

/// Atomically publish the current run state for a session.
pub fn write(
  session_id: String,
  status: String,
  steps: List(Step),
  text: String,
  context_tokens: Int,
  net_events: List(AuditEvent),
) -> Nil {
  case dir() {
    Error(_) -> Nil
    Ok(d) -> {
      let target = path(d, session_id)
      let tmp = target <> ".tmp"
      let content =
        json.to_string(agent.run_json(
          status,
          steps,
          text,
          context_tokens,
          net_events,
        ))
      case simplifile.write(tmp, content) {
        Ok(_) -> {
          let _ = simplifile.rename(tmp, target)
          Nil
        }
        Error(_) -> Nil
      }
    }
  }
}

/// Raw run JSON for a session, or `Error` if no run has started.
pub fn read_raw(session_id: String) -> Result(String, Nil) {
  use d <- result.try(dir())
  simplifile.read(path(d, session_id)) |> result.replace_error(Nil)
}

/// The session's current `#(status, text)` — used by `collect` to wait on a
/// subagent and read back its result.
pub fn read_status_text(session_id: String) -> Result(#(String, String), Nil) {
  use body <- result.try(read_raw(session_id))
  json.parse(body, {
    use status <- decode.field("status", decode.string)
    use text <- decode.field("text", decode.string)
    decode.success(#(status, text))
  })
  |> result.replace_error(Nil)
}
