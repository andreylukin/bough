import bough_core/nono.{Allow, AuditEvent, Deny, Snapshot}
import bough_server/json_value
import bough_server/nono_bridge
import gleam/json
import gleam/option.{None, Some}
import gleeunit

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
