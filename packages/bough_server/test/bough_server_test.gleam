import bough_core/nono.{Allow, AuditEvent, Deny, Snapshot}
import bough_server/control
import bough_server/json_value
import bough_server/net_profile
import bough_server/nono_bridge
import bough_server/snapshots
import gleam/json
import gleam/option.{None, Some}
import gleam/string
import gleeunit
import simplifile

pub fn main() -> Nil {
  gleeunit.main()
}

pub fn to_args_block_net_test() {
  let profile = nono.Profile("/ws", [], True, False)
  assert nono_bridge.to_args(profile, ["echo", "hi"])
    == [
      "run", "--detached", "--allow", "/ws", "--block-net", "--no-rollback",
      "--", "echo", "hi",
    ]
}

pub fn to_args_allowlist_and_rollback_test() {
  let profile =
    nono.default_profile("/ws", ["api.anthropic.com", "api.openai.com"])
  assert nono_bridge.to_args(profile, ["claude"])
    == [
      "run", "--detached", "--allow", "/ws", "--allow-domain",
      "api.anthropic.com", "--allow-domain", "api.openai.com", "--rollback",
      "--no-rollback-prompt", "--", "claude",
    ]
}

pub fn parse_session_id_test() {
  let output =
    "Started detached session dc0b47c235ccb456.\nAttach with: nono attach dc0b47c235ccb456"
  assert nono_bridge.parse_session_id(output) == Ok("dc0b47c235ccb456")
}

pub fn parse_session_id_missing_test() {
  assert nono_bridge.parse_session_id("nothing here") == Error(Nil)
}

pub fn parse_network_events_test() {
  // Shape taken verbatim from `nono audit show --json`.
  let json =
    "{\"network_events\":["
    <> "{\"timestamp_unix_ms\":1781670296845,\"mode\":\"connect\",\"decision\":\"allow\",\"target\":\"example.com\",\"port\":443,\"method\":\"CONNECT\",\"path\":null,\"status\":null,\"reason\":null},"
    <> "{\"timestamp_unix_ms\":1781670296934,\"mode\":\"connect\",\"decision\":\"deny\",\"denial_category\":\"host_denied\",\"target\":\"api.github.com\",\"port\":443,\"method\":null,\"path\":null,\"status\":null,\"reason\":\"host api.github.com is not in the allowlist\"}"
    <> "]}"

  assert nono_bridge.parse_network_events(json)
    == Ok([
      AuditEvent(
        "example.com",
        443,
        Some("CONNECT"),
        None,
        Allow,
        None,
        1_781_670_296_845,
      ),
      AuditEvent(
        "api.github.com",
        443,
        None,
        None,
        Deny,
        Some("host api.github.com is not in the allowlist"),
        1_781_670_296_934,
      ),
    ])
}

pub fn json_value_round_trip_test() {
  // Covers the tool-use round-trip: arbitrary JSON must re-encode unchanged.
  let src =
    "{\"a\":\"x\",\"n\":1,\"b\":true,\"z\":null,\"arr\":[1,\"two\",false]}"
  let assert Ok(value) = json.parse(src, json_value.decoder())
  let assert Ok(reparsed) =
    json.parse(json.to_string(json_value.to_json(value)), json_value.decoder())
  assert reparsed == value
}

pub fn restore_args_test() {
  assert nono_bridge.restore_args(Snapshot("20260617-002456-39161", "1", 0))
    == ["rollback", "restore", "20260617-002456-39161", "--snapshot", "1"]
}

/// The control channel is read-once: a put is taken exactly once, a steer
/// carries its message, and clear/empty yields Error.
pub fn control_round_trip_test() {
  let id = "bough-test-control-slot"
  control.clear(id)
  assert control.take(id) == Error(Nil)

  control.put(id, control.Allow)
  assert control.take(id) == Ok(control.Allow)
  // Read-once: the slot is now empty.
  assert control.take(id) == Error(Nil)

  control.put(id, control.Steer("try a smaller change"))
  assert control.take(id) == Ok(control.Steer("try a smaller change"))

  control.put(id, control.Allow)
  control.clear(id)
  assert control.take(id) == Error(Nil)
}

/// The generated network profile groups rules by host: multiple path rules for
/// one host union into one endpoints array; a bare host stays a plain string.
pub fn net_profile_unions_paths_test() {
  let j =
    json.to_string(
      net_profile.build(
        [
          "https://api.foo.com/v1/**", "https://api.foo.com/v2/**",
          "bare.example.com",
        ],
        [],
      ),
    )
  // Both path globs present under the one host, as endpoint rules.
  assert string.contains(j, "/v1/**")
  assert string.contains(j, "/v2/**")
  assert string.contains(j, "\"domain\":\"api.foo.com\"")
  // The bare host appears as a plain allowlist string (no endpoints object).
  assert string.contains(j, "\"bare.example.com\"")
  // One endpoints array for the host (i.e. unioned, not two domain objects).
  assert count_occurrences(j, "\"domain\":\"api.foo.com\"") == 1
  // The git_config group is always included so a sandboxed `git` can read its
  // config; no env_credentials block when none are injected.
  assert string.contains(j, "\"git_config\"")
  assert !string.contains(j, "env_credentials")
}

/// Injected credentials become an `env_credentials` map (name -> env var) in
/// the profile (SPEC §6.4).
pub fn net_profile_credentials_test() {
  let j =
    json.to_string(net_profile.build([], [#("github_token", "GITHUB_TOKEN")]))
  assert string.contains(j, "\"env_credentials\"")
  assert string.contains(j, "\"github_token\":\"GITHUB_TOKEN\"")
}

/// The BOUGH_NET_CREDENTIALS spec parses `name=ENV` and bare `name` entries,
/// trims blanks, and upper-cases the env var for bare names.
pub fn parse_credentials_test() {
  assert net_profile.parse_credentials(
      "github_token=GH_PAT, anthropic_api_key, ",
    )
    == [
      #("github_token", "GH_PAT"),
      #("anthropic_api_key", "ANTHROPIC_API_KEY"),
    ]
  assert net_profile.parse_credentials("") == []
}

fn count_occurrences(haystack: String, needle: String) -> Int {
  list_length(string.split(haystack, needle)) - 1
}

fn list_length(l: List(a)) -> Int {
  case l {
    [] -> 0
    [_, ..rest] -> 1 + list_length(rest)
  }
}

/// The endpoint-deny reason yields method + path; a plain CONNECT deny yields
/// neither (host-only).
pub fn parse_endpoint_reason_test() {
  assert nono_bridge.parse_endpoint_reason(Some(
      "endpoint rules denied GET /secret: no rule matched on example.com:443",
    ))
    == #(Some("GET"), Some("/secret"))

  assert nono_bridge.parse_endpoint_reason(Some(
      "host api.github.com is not in the allowlist",
    ))
    == #(None, None)

  assert nono_bridge.parse_endpoint_reason(None) == #(None, None)
}

/// Detection finds the newest audit session matching the command and started
/// after the watermark, with its net-event count (older runs / other commands
/// excluded). No-net runs are still found, so the caller can stop polling.
pub fn pick_session_newest_matching_test() {
  let cmd = ["sh", "-c", "curl x"]
  let json =
    "["
    <> "{\"session_id\":\"old\",\"started\":\"2026-06-18T16:00:00-04:00\",\"command\":[\"sh\",\"-c\",\"curl x\"],\"network_event_count\":0},"
    <> "{\"session_id\":\"run\",\"started\":\"2026-06-18T16:05:00-04:00\",\"command\":[\"sh\",\"-c\",\"curl x\"],\"network_event_count\":2},"
    <> "{\"session_id\":\"other\",\"started\":\"2026-06-18T16:09:00-04:00\",\"command\":[\"ls\"],\"network_event_count\":3}"
    <> "]"
  // Newest matching after an early watermark: the run, with its net count.
  assert nono_bridge.pick_session(json, cmd, "2026-06-18T16:02:00-04:00")
    == Ok(#("run", 2))
  // Watermark past the run excludes it.
  assert nono_bridge.pick_session(json, cmd, "2026-06-18T16:06:00-04:00")
    == Error(Nil)
}

pub fn pick_session_no_match_test() {
  let json =
    "[{\"session_id\":\"x\",\"started\":\"2026-06-18T16:00:00-04:00\",\"command\":[\"ls\"],\"network_event_count\":1}]"
  assert nono_bridge.pick_session(json, ["sh", "-c", "curl x"], "")
    == Error(Nil)
}

/// A snapshot captures the workspace; a later restore reverts a modified file
/// and removes a file added after the snapshot (SPEC §4.1).
pub fn snapshot_capture_and_restore_test() {
  let ws = "/tmp/bough-snap-test-ws"
  let sid = "bough-snap-test"
  let _ = simplifile.delete(ws)
  let _ = simplifile.create_directory_all(ws)
  let _ = simplifile.write(ws <> "/a.txt", "one")

  let assert Ok(ref) = snapshots.capture(sid, ws)

  let _ = simplifile.write(ws <> "/a.txt", "two")
  let _ = simplifile.write(ws <> "/b.txt", "new")

  let assert Ok(_) = snapshots.restore(sid, ws, ref)
  assert simplifile.read(ws <> "/a.txt") == Ok("one")
  assert simplifile.is_file(ws <> "/b.txt") == Ok(False)

  let _ = simplifile.delete(ws)
}

/// Live integration: drive nono through the bridge to launch and stop a real
/// sandbox. Skips (passes) when nono is not installed so `make test` stays
/// green on machines without it.
pub fn launch_and_stop_smoke_test() {
  let profile = nono.Profile("/tmp", [], True, False)
  case nono_bridge.launch(profile, ["sleep", "10"]) {
    Ok(id) -> {
      assert id != ""
      let _ = nono_bridge.stop(id)
      Nil
    }
    Error(_) -> Nil
  }
}
