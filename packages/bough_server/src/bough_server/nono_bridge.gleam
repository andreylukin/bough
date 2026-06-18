//// Bridge to the nono CLI. Since nono has no BEAM SDK, bough drives the CLI and
//// (later) parses its session registry, proxy audit log, and rollback metadata
//// back into `bough_core/nono` types (SPEC.md §6).
////
//// `to_args` and `parse_session_id` are pure (unit-tested); `launch`/`stop`
//// shell out via shellout.

import bough_core/nono.{
  type AuditEvent, type Profile, type Snapshot, Allow, AuditEvent, Deny,
}
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{None}
import gleam/result
import gleam/string
import shellout

/// Build `nono run` arguments from a capability profile. Always detached so the
/// supervisor keeps the agent running while clients attach/detach (SPEC.md §8).
pub fn to_args(profile: Profile, command: List(String)) -> List(String) {
  let rollback = case profile.rollback {
    True -> ["--rollback", "--no-rollback-prompt"]
    False -> ["--no-rollback"]
  }

  list.flatten([
    ["run", "--detached", "--allow", profile.workspace],
    net_flags(profile),
    rollback,
    ["--"],
    command,
  ])
}

fn net_flags(profile: Profile) -> List(String) {
  case profile.block_net {
    True -> ["--block-net"]
    False ->
      list.flat_map(profile.allow_domains, fn(d) { ["--allow-domain", d] })
  }
}

/// Run a command foreground inside the sandbox and capture its combined output.
/// Used for one-shot tool execution (e.g. the `bash` tool). Returns whatever
/// the command printed, even on non-zero exit, so the agent can see errors.
pub fn run(profile: Profile, command: List(String)) -> String {
  case shellout.command("nono", run_args(profile, command), profile.workspace, []) {
    Ok(output) -> output
    Error(#(_code, output)) -> output
  }
}

/// Like `run`, but also returns the exit code (0 on success). The engine needs
/// it: RUN-step success and the CHECK gate are decided by exit status, not just
/// captured output (SPEC.md §5.3).
pub fn run_result(profile: Profile, command: List(String)) -> #(Int, String) {
  case shellout.command("nono", run_args(profile, command), profile.workspace, []) {
    Ok(output) -> #(0, output)
    Error(#(code, output)) -> #(code, output)
  }
}

pub fn run_args(profile: Profile, command: List(String)) -> List(String) {
  list.flatten([
    ["run", "-s", "--allow", profile.workspace, "--allow-cwd"],
    net_flags(profile),
    ["--no-rollback", "--"],
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

// --- Network audit feed (SPEC.md §7) -------------------------------------

/// Read the proxy audit log for a session as `AuditEvent`s for the network
/// side pane. `session_id` is nono's audit session id (YYYYMMDD-HHMMSS-PID).
pub fn audit_events(session_id: String) -> Result(List(AuditEvent), String) {
  case
    shellout.command("nono", ["audit", "show", session_id, "--json"], ".", [])
  {
    Ok(output) -> parse_network_events(output)
    Error(#(_code, message)) -> Error(message)
  }
}

/// Parse `nono audit show --json` into network `AuditEvent`s.
pub fn parse_network_events(json: String) -> Result(List(AuditEvent), String) {
  json.parse(json, audit_decoder())
  |> result.replace_error("could not parse nono audit JSON")
}

fn audit_decoder() -> decode.Decoder(List(AuditEvent)) {
  use events <- decode.field(
    "network_events",
    decode.list(network_event_decoder()),
  )
  decode.success(events)
}

fn network_event_decoder() -> decode.Decoder(AuditEvent) {
  use host <- decode.field("target", decode.string)
  use port <- decode.field("port", decode.int)
  use method <- decode.field("method", decode.optional(decode.string))
  use path <- decode.field("path", decode.optional(decode.string))
  use decision_s <- decode.field("decision", decode.string)
  use reason <- decode.field("reason", decode.optional(decode.string))
  use timestamp <- decode.field("timestamp_unix_ms", decode.int)
  case nono.decision_from_string(decision_s) {
    Ok(decision) ->
      decode.success(AuditEvent(
        host: host,
        port: port,
        method: method,
        path: path,
        decision: decision,
        reason: reason,
        timestamp: timestamp,
      ))
    Error(_) ->
      decode.failure(
        AuditEvent(host, port, None, None, Allow, None, timestamp),
        "NetDecision",
      )
  }
}

// --- Network denials (the leash, SPEC.md §7) -----------------------------

/// The distinct hosts the just-run sandboxed `command` was DENIED outbound
/// access to, read from nono's audit trail. Drives the network-approval gate:
/// a denied host is surfaced to the human, and on approval added to the
/// allowlist for a retry. Empty if there were no denials (or audit is
/// unavailable — non-fatal).
pub fn denied_hosts(command: List(String)) -> List(String) {
  case latest_session(command) {
    Error(_) -> []
    Ok(session_id) ->
      case audit_events(session_id) {
        Ok(events) ->
          events
          |> list.filter_map(fn(e) {
            case e.decision {
              Deny -> Ok(e.host)
              Allow -> Error(Nil)
            }
          })
          |> list.unique
        Error(_) -> []
      }
  }
}

/// The most recent audit session whose command matches `command` and which
/// recorded network events — i.e. the run we just performed.
fn latest_session(command: List(String)) -> Result(String, Nil) {
  case shellout.command("nono", ["audit", "list", "--today", "--json"], ".", []) {
    Ok(out) -> pick_session(out, command)
    Error(_) -> Error(Nil)
  }
}

/// Pure: pick the matching session id from `nono audit list --json` output.
pub fn pick_session(out: String, command: List(String)) -> Result(String, Nil) {
  use entries <- result.try(
    json.parse(out, decode.list(audit_list_decoder()))
    |> result.replace_error(Nil),
  )
  entries
  |> list.filter(fn(e) { e.command == command && e.net_count > 0 })
  |> list.sort(fn(a, b) { string.compare(a.started, b.started) })
  |> list.reverse
  |> list.first
  |> result.map(fn(e) { e.session_id })
}

type AuditListEntry {
  AuditListEntry(
    session_id: String,
    started: String,
    command: List(String),
    net_count: Int,
  )
}

fn audit_list_decoder() -> decode.Decoder(AuditListEntry) {
  use session_id <- decode.field("session_id", decode.string)
  use started <- decode.field("started", decode.string)
  use command <- decode.field("command", decode.list(decode.string))
  use net_count <- decode.field("network_event_count", decode.int)
  decode.success(AuditListEntry(session_id:, started:, command:, net_count:))
}

// --- Snapshots (SPEC.md §4.1) --------------------------------------------

/// Restore a snapshot before continuing from a forked node.
pub fn restore(snapshot: Snapshot) -> Result(Nil, String) {
  case shellout.command("nono", restore_args(snapshot), ".", []) {
    Ok(_) -> Ok(Nil)
    Error(#(_code, message)) -> Error(message)
  }
}

pub fn restore_args(snapshot: Snapshot) -> List(String) {
  [
    "rollback", "restore", snapshot.session_id, "--snapshot",
    snapshot.reference,
  ]
}

/// On-demand, per-write-turn snapshot capture is not a nono CLI primitive —
/// nono snapshots at session boundaries under `--rollback`. Cadence is an open
/// design question (SPEC.md §11); deferred.
pub fn snapshot(_session_id: String) -> Result(Snapshot, String) {
  Error("nono_bridge.snapshot: deferred — see SPEC.md §11")
}
