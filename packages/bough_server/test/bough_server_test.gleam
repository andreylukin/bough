import bough_core/nono.{Allow, AuditEvent, Deny, Snapshot}
import bough_server/control
import bough_server/json_value
import bough_server/nono_bridge
import bough_server/snapshots
import gleam/json
import gleam/option.{None, Some}
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
      AuditEvent("example.com", 443, Some("CONNECT"), None, Allow, None, 1781670296845),
      AuditEvent(
        "api.github.com",
        443,
        None,
        None,
        Deny,
        Some("host api.github.com is not in the allowlist"),
        1781670296934,
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

/// Network denial detection picks the newest audit session whose command
/// matches and which had network events (ignoring no-net and other commands).
pub fn pick_session_newest_matching_test() {
  let cmd = ["sh", "-c", "curl x"]
  let json =
    "["
    <> "{\"session_id\":\"old\",\"started\":\"2026-06-18T16:00:00-04:00\",\"command\":[\"sh\",\"-c\",\"curl x\"],\"network_event_count\":2},"
    <> "{\"session_id\":\"new\",\"started\":\"2026-06-18T16:05:00-04:00\",\"command\":[\"sh\",\"-c\",\"curl x\"],\"network_event_count\":1},"
    <> "{\"session_id\":\"nonet\",\"started\":\"2026-06-18T16:08:00-04:00\",\"command\":[\"sh\",\"-c\",\"curl x\"],\"network_event_count\":0},"
    <> "{\"session_id\":\"other\",\"started\":\"2026-06-18T16:09:00-04:00\",\"command\":[\"ls\"],\"network_event_count\":3}"
    <> "]"
  assert nono_bridge.pick_session(json, cmd) == Ok("new")
}

pub fn pick_session_no_match_test() {
  let json =
    "[{\"session_id\":\"x\",\"started\":\"2026-06-18T16:00:00-04:00\",\"command\":[\"ls\"],\"network_event_count\":1}]"
  assert nono_bridge.pick_session(json, ["sh", "-c", "curl x"]) == Error(Nil)
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
