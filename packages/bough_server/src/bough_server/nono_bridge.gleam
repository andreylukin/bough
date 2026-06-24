//// Bridge to the nono CLI. Since nono has no BEAM SDK, bough drives the CLI and
//// (later) parses its session registry, proxy audit log, and rollback metadata
//// back into `bough_core/nono` types (SPEC.md §6).
////
//// `to_args` and `parse_session_id` are pure (unit-tested); `launch`/`stop`
//// shell out via shellout.

import bough_core/nono.{
  type AuditEvent, type Group, type GroupDetail, type GroupPath, type Profile,
  type Snapshot, Allow, AuditEvent, Deny, Group, GroupDetail, GroupPath,
}
import envoy
import gleam/dynamic/decode
import gleam/json
import gleam/list
import gleam/option.{type Option, None, Some}
import gleam/order.{Gt}
import gleam/result
import gleam/string
import shellout
import simplifile

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
  case
    shellout.command(
      "nono",
      run_args(profile, [], command),
      profile.workspace,
      [],
    )
  {
    Ok(output) -> output
    Error(#(_code, output)) -> output
  }
}

/// Like `run`, but also returns the exit code (0 on success). The engine needs
/// it: RUN-step success and the CHECK gate are decided by exit status, not just
/// captured output (SPEC.md §5.3). `reads` are extra single-file read grants
/// (e.g. a staged write source outside the workspace).
pub fn run_result(
  profile: Profile,
  reads: List(String),
  command: List(String),
) -> #(Int, String) {
  case
    shellout.command(
      "nono",
      run_args(profile, reads, command),
      profile.workspace,
      [],
    )
  {
    Ok(output) -> #(0, output)
    Error(#(code, output)) -> #(code, output)
  }
}

pub fn run_args(
  profile: Profile,
  reads: List(String),
  command: List(String),
) -> List(String) {
  list.flatten([
    ["run", "-s", "--allow", profile.workspace, "--allow-cwd"],
    toolchain_reads(),
    read_flags(reads),
    net_flags(profile),
    ["--no-rollback", "--"],
    command,
  ])
}

/// Toolchain dirs (relative to `$HOME`) that hold language-tool binaries —
/// cargo, go, node, pyenv, etc. The sandbox grants only the workspace by
/// default, so a RUN/CHECK that shells out to one of these needs read access to
/// find it on PATH (nono docs: "Enabling LSPs, Linters, and Dev Tools").
const toolchain_dirs = [
  ".cargo/bin", "go/bin", ".pyenv/shims", ".pyenv/bin", ".rbenv/shims",
  ".rbenv/bin", ".ghcup/bin", ".nvm", ".local/share/fnm", ".local/share/pnpm",
  ".local/bin",
]

/// `--read` flag pairs for the toolchain dirs that exist under `$HOME`. Read,
/// not allow: enough for PATH lookup, no write access granted.
pub fn toolchain_reads() -> List(String) {
  case envoy.get("HOME") {
    Ok(home) if home != "" ->
      toolchain_dirs
      |> list.map(fn(d) { home <> "/" <> d })
      |> list.filter(fn(p) { simplifile.is_directory(p) == Ok(True) })
      |> list.flat_map(fn(p) { ["--read", p] })
    _ -> []
  }
}

/// `--read-file` grants for files the sandboxed command may read but not write
/// (used to feed a staged write whose source lives outside the workspace).
fn read_flags(reads: List(String)) -> List(String) {
  list.flat_map(reads, fn(r) { ["--read-file", r] })
}

/// Validate a generated profile against the installed nono
/// (`nono profile validate <path>`). Surfaces schema drift loudly instead of
/// letting `nono run` reject it opaquely. `Ok` if valid; `Error` with nono's
/// complaint otherwise.
pub fn validate_profile(path: String) -> Result(Nil, String) {
  case shellout.command("nono", ["profile", "validate", path], ".", []) {
    Ok(_) -> Ok(Nil)
    Error(#(_code, message)) -> Error(message)
  }
}

/// Run a command in the sandbox under a generated nono profile (the network
/// leash, SPEC §7). The profile supplies default-deny network + the session's
/// allow rules (unioned per host); `--allow` supplies workspace filesystem.
/// Returns the exit code and combined output.
pub fn run_in_profile(
  workspace: String,
  profile_path: String,
  reads: List(String),
  command: List(String),
) -> #(Int, String) {
  run_celled(workspace, Some(profile_path), reads, command)
}

/// Wrap an arbitrary command in one nono cell scoped to `workspace` (read/write
/// + the toolchain dirs on PATH), optionally under the session `profile`
/// (capability groups + net leash + credentials). Running the monty sidecar
/// this way puts its in-process `read`/`write`/`edit` — not just `bash` — behind
/// the kernel sandbox, and children inherit the cell (SPEC §6). One arg builder
/// for both the typed RUN path and code-mode, so the two can't drift.
pub fn run_celled(
  workspace: String,
  profile: Option(String),
  reads: List(String),
  command: List(String),
) -> #(Int, String) {
  let profile_flags = case profile {
    Some(path) -> ["--profile", path]
    None -> []
  }
  let args =
    list.flatten([
      ["run", "-s", "--allow", workspace, "--allow-cwd"],
      profile_flags,
      toolchain_reads(),
      read_flags(reads),
      ["--no-rollback", "--"],
      command,
    ])
  case shellout.command("nono", args, workspace, []) {
    Ok(output) -> #(0, output)
    Error(#(code, output)) -> #(code, output)
  }
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

// --- Capability-group catalog (SPEC.md §7) -------------------------------

/// The nono policy-group catalog for this host. `locked` is set for groups nono
/// always applies (the `required` denies + the default base), which the human
/// can't toggle off. Empty if nono is unavailable or its output unparseable, so
/// the picker degrades to "no groups" rather than erroring. nono already filters
/// the catalog to the host platform, so no OS filtering is needed here.
pub fn list_groups() -> List(Group) {
  let locked = default_group_names()
  case shellout.command("nono", ["profile", "groups", "--json"], ".", []) {
    Ok(output) ->
      case json.parse(crop_to(output, "["), groups_decoder()) {
        Ok(groups) ->
          list.map(groups, fn(g) {
            let #(name, description, platform, required) = g
            Group(
              name: name,
              description: description,
              platform: platform,
              locked: required || list.contains(locked, name),
            )
          })
        Error(_) -> []
      }
    Error(_) -> []
  }
}

/// Names of the groups in nono's `default` profile — always applied, so locked.
fn default_group_names() -> List(String) {
  case
    shellout.command("nono", ["profile", "show", "default", "--json"], ".", [])
  {
    Ok(output) ->
      json.parse(crop_to(output, "{"), default_include_decoder())
      |> result.unwrap([])
    Error(_) -> []
  }
}

/// nono may print an update banner before its JSON; slice from the first opening
/// bracket so the payload parses either way.
fn crop_to(output: String, bracket: String) -> String {
  string.crop(output, bracket)
}

fn groups_decoder() -> decode.Decoder(List(#(String, String, String, Bool))) {
  decode.list({
    use name <- decode.field("name", decode.string)
    use description <- decode.field("description", decode.string)
    use platform <- decode.field("platform", decode.string)
    use required <- decode.field("required", decode.bool)
    decode.success(#(name, description, platform, required))
  })
}

fn default_include_decoder() -> decode.Decoder(List(String)) {
  use include <- decode.subfield(
    ["groups", "include"],
    decode.list(decode.string),
  )
  decode.success(include)
}

/// The full contents of one group: the paths it grants or denies. Used by the
/// TUI's group inspector. `Error` if nono is unavailable or the name is unknown.
pub fn group_detail(name: String) -> Result(GroupDetail, String) {
  case shellout.command("nono", ["profile", "groups", name, "--json"], ".", []) {
    Ok(output) ->
      json.parse(crop_to(output, "{"), group_detail_decoder())
      |> result.replace_error("could not parse group detail for " <> name)
    Error(#(_code, message)) -> Error(message)
  }
}

fn group_detail_decoder() -> decode.Decoder(GroupDetail) {
  use name <- decode.field("name", decode.string)
  use description <- decode.field("description", decode.string)
  // `allow`/`deny` may be absent or null; wrap in `optional` to tolerate both.
  use allow <- decode.optional_field("allow", None, decode.optional(allow_decoder()))
  use deny <- decode.optional_field("deny", None, decode.optional(deny_decoder()))
  let #(read, write, rw) = option.unwrap(allow, #([], [], []))
  let deny_paths = option.unwrap(deny, [])
  let paths =
    list.flatten([
      label_paths("read", read),
      label_paths("rw", rw),
      label_paths("write", write),
      label_paths("deny", deny_paths),
    ])
  decode.success(GroupDetail(name: name, description: description, paths: paths))
}

fn allow_decoder() -> decode.Decoder(#(List(String), List(String), List(String))) {
  use read <- decode.optional_field("read", [], raw_paths())
  use write <- decode.optional_field("write", [], raw_paths())
  use rw <- decode.optional_field("readwrite", [], raw_paths())
  decode.success(#(read, write, rw))
}

fn deny_decoder() -> decode.Decoder(List(String)) {
  use access <- decode.optional_field("access", [], raw_paths())
  decode.success(access)
}

fn raw_paths() -> decode.Decoder(List(String)) {
  decode.list({
    use raw <- decode.field("raw", decode.string)
    decode.success(raw)
  })
}

fn label_paths(access: String, paths: List(String)) -> List(GroupPath) {
  list.map(paths, fn(p) { GroupPath(access: access, path: p) })
}

/// Toggleable group names that would grant one of the `targets` — the paths the
/// denied step was actually trying to touch. A group matches only when a target
/// is the group's grant or sits *under* it (not merely a shared ancestor), so a
/// coarse denial like `~/Library` doesn't pull in every `~/Library/*` group.
/// Fetches each toggleable group's detail once; `[]` if nono is unavailable.
pub fn groups_for_paths(targets: List(String)) -> List(String) {
  let home = envoy.get("HOME") |> result.unwrap("")
  let wanted = list.map(targets, fn(t) { normalize_path(t, home) })
  list_groups()
  |> list.filter(fn(g) { !g.locked })
  |> list.filter_map(fn(g) {
    case group_detail(g.name) {
      Ok(d) -> {
        let grants =
          d.paths
          |> list.filter(fn(p) { p.access != "deny" })
          |> list.map(fn(p) { normalize_path(p.path, home) })
        case
          list.any(wanted, fn(t) { list.any(grants, fn(gr) { covers(gr, t) }) })
        {
          True -> Ok(g.name)
          False -> Error(Nil)
        }
      }
      Error(_) -> Error(Nil)
    }
  })
}

fn normalize_path(raw: String, home: String) -> String {
  raw
  |> string.replace("$HOME", home)
  |> string.replace("~", home)
}

/// True when `target` is `grant` itself or a path under it — i.e. enabling the
/// group would actually grant the denied access. Empty paths never match.
fn covers(grant: String, target: String) -> Bool {
  grant != ""
  && { target == grant || string.starts_with(target, grant <> "/") }
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
  // `port` is null on intercepted L7 events (only the CONNECT carries it), so
  // tolerate null/absent — a strict int decode here would fail the whole list
  // and drop every event in the session, including the denials.
  use port_opt <- decode.field("port", decode.optional(decode.int))
  use method <- decode.field("method", decode.optional(decode.string))
  use path <- decode.field("path", decode.optional(decode.string))
  use decision_s <- decode.field("decision", decode.string)
  use reason <- decode.field("reason", decode.optional(decode.string))
  use timestamp <- decode.field("timestamp_unix_ms", decode.int)
  let port = option.unwrap(port_opt, 0)
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

/// A denied outbound request: the host, plus the method/path when nono was
/// intercepting that host at L7 (otherwise just the host, from a CONNECT deny).
pub type Denial {
  Denial(host: String, method: Option(String), path: Option(String))
}

/// The distinct requests denied in the given audit session, host plus
/// method/path when intercepted. Empty if none (or the session is unreadable).
pub fn denials_of(session_id: String) -> List(Denial) {
  case audit_events(session_id) {
    Ok(events) ->
      events
      |> list.filter_map(fn(e) {
        case e.decision {
          Deny -> Ok(to_denial(e))
          Allow -> Error(Nil)
        }
      })
      |> list.unique
    Error(_) -> []
  }
}

/// The newest `started` among audit sessions for `command` right now, or "" if
/// none. Capture before running so `find_session` can ignore older sessions.
pub fn session_watermark(command: List(String)) -> String {
  case
    shellout.command("nono", ["audit", "list", "--today", "--json"], ".", [])
  {
    Error(_) -> ""
    Ok(out) ->
      case json.parse(out, decode.list(audit_list_decoder())) {
        Error(_) -> ""
        Ok(entries) ->
          entries
          |> list.filter(fn(e) { e.command == command })
          |> list.map(fn(e) { e.started })
          |> list.sort(string.compare)
          |> list.last
          |> result.unwrap("")
      }
  }
}

/// The audit session for the run we just did: the newest matching `command` and
/// started after the watermark. Returns its id and network-event count, so the
/// caller can poll until it appears (the audit is flushed slightly after the
/// command exits) and then inspect denials only when there were net events.
pub fn find_session(
  command: List(String),
  after: String,
) -> Result(#(String, Int), Nil) {
  case
    shellout.command("nono", ["audit", "list", "--today", "--json"], ".", [])
  {
    Ok(out) -> pick_session(out, command, after)
    Error(_) -> Error(Nil)
  }
}

fn to_denial(e: AuditEvent) -> Denial {
  // Prefer nono's structured method/path fields from the audit JSON. Only when
  // both are absent fall back to parsing the prose reason — on some L7
  // (intercepted) denies nono carries them only in the reason string, e.g.
  // "endpoint rules denied GET /secret: no rule matched on host:443"; a plain
  // CONNECT deny ("host X is not in the allowlist") has neither.
  case e.method, e.path {
    None, None -> {
      let #(method, path) = parse_endpoint_reason(e.reason)
      Denial(host: e.host, method: method, path: path)
    }
    _, _ -> Denial(host: e.host, method: e.method, path: e.path)
  }
}

/// Pure: pull `#(method, path)` out of an endpoint-deny reason string, if present.
pub fn parse_endpoint_reason(
  reason: Option(String),
) -> #(Option(String), Option(String)) {
  case reason {
    None -> #(None, None)
    Some(r) ->
      case string.split_once(r, "denied ") {
        Error(_) -> #(None, None)
        Ok(#(_, rest)) -> {
          let head = case string.split_once(rest, ":") {
            Ok(#(h, _)) -> h
            Error(_) -> rest
          }
          case string.split_once(string.trim(head), " ") {
            Ok(#(method, path)) -> #(Some(method), Some(string.trim(path)))
            Error(_) -> #(None, None)
          }
        }
      }
  }
}

/// Pure: pick the run's session from `nono audit list --json` output — the
/// newest entry matching `command` and started strictly after `after`. Returns
/// its id and network-event count (no net-count filter, so a no-network run is
/// found too, which lets the caller stop polling).
pub fn pick_session(
  out: String,
  command: List(String),
  after: String,
) -> Result(#(String, Int), Nil) {
  use entries <- result.try(
    json.parse(out, decode.list(audit_list_decoder()))
    |> result.replace_error(Nil),
  )
  entries
  |> list.filter(fn(e) {
    e.command == command && string.compare(e.started, after) == Gt
  })
  |> list.sort(fn(a, b) { string.compare(a.started, b.started) })
  |> list.reverse
  |> list.first
  |> result.map(fn(e) { #(e.session_id, e.net_count) })
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
  ["rollback", "restore", snapshot.session_id, "--snapshot", snapshot.reference]
}

/// On-demand, per-write-turn snapshot capture is not a nono CLI primitive —
/// nono snapshots at session boundaries under `--rollback`. Cadence is an open
/// design question (SPEC.md §11); deferred.
pub fn snapshot(_session_id: String) -> Result(Snapshot, String) {
  Error("nono_bridge.snapshot: deferred — see SPEC.md §11")
}
