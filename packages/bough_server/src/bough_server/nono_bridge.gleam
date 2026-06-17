//// Bridge to the nono CLI. Since nono has no BEAM SDK, bough drives the CLI and
//// (later) parses its session registry, proxy audit log, and rollback metadata
//// back into `bough_core/nono` types (SPEC.md §6).
////
//// `to_args` and `parse_session_id` are pure (unit-tested); `launch`/`stop`
//// shell out via shellout.

import bough_core/nono.{type AuditEvent, type Profile, type Snapshot}
import gleam/list
import gleam/result
import gleam/string
import shellout

/// Build `nono run` arguments from a capability profile. Always detached so the
/// supervisor keeps the agent running while clients attach/detach (SPEC.md §8).
pub fn to_args(profile: Profile, command: List(String)) -> List(String) {
  let filesystem = ["--allow", profile.workspace]

  let network = case profile.block_net {
    True -> ["--block-net"]
    False ->
      list.flat_map(profile.allow_domains, fn(d) { ["--allow-domain", d] })
  }

  let rollback = case profile.rollback {
    True -> ["--rollback", "--no-rollback-prompt"]
    False -> ["--no-rollback"]
  }

  list.flatten([
    ["run", "--detached"],
    filesystem,
    network,
    rollback,
    ["--"],
    command,
  ])
}

/// Extract the session id from `nono run --detached` output
/// ("Started detached session <id>.").
pub fn parse_session_id(output: String) -> Result(String, Nil) {
  output
  |> string.split("\n")
  |> list.filter_map(fn(line) {
    case string.split_once(line, "Started detached session ") {
      Ok(#(_, rest)) -> Ok(string.trim(string.replace(rest, ".", "")))
      Error(_) -> Error(Nil)
    }
  })
  |> list.first
}

/// Launch a sandboxed agent. Returns the nono session id.
pub fn launch(profile: Profile, command: List(String)) -> Result(String, String) {
  case shellout.command("nono", to_args(profile, command), ".", []) {
    Ok(output) ->
      parse_session_id(output)
      |> result.replace_error("no session id in nono output:\n" <> output)
    Error(#(_code, message)) -> Error(message)
  }
}

/// `nono stop <id>` — terminate a session cleanly.
pub fn stop(session_id: String) -> Result(Nil, String) {
  case shellout.command("nono", ["stop", session_id], ".", []) {
    Ok(_) -> Ok(Nil)
    Error(#(_code, message)) -> Error(message)
  }
}

// --- Not yet implemented (later slices) ----------------------------------

/// Tail the proxy audit log for a session, feeding the network side pane
/// (SPEC.md §7).
pub fn audit_events(_session_id: String) -> Result(List(AuditEvent), String) {
  Error("nono_bridge.audit_events: not implemented")
}

/// Capture a rollback snapshot of the current filesystem state (SPEC.md §4.1).
pub fn snapshot(_session_id: String) -> Result(Snapshot, String) {
  Error("nono_bridge.snapshot: not implemented")
}

/// Restore a snapshot before continuing from a forked node (SPEC.md §4.1).
pub fn restore(_snapshot: Snapshot) -> Result(Nil, String) {
  Error("nono_bridge.restore: not implemented")
}
