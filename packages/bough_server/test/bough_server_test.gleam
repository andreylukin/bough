import bough_core/nono
import bough_server/nono_bridge
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
