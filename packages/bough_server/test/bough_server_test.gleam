import bough_core/artifact.{Code, Collect}
import bough_core/nono.{Allow, AuditEvent, Deny, Group, Snapshot}
import bough_core/session
import bough_server/agent
import bough_server/control
import bough_server/engine
import bough_server/router
import bough_server/json_value
import bough_server/net_profile
import bough_server/nono_bridge
import bough_server/providers
import bough_server/seatbelt
import bough_server/snapshots
import gleam/dict
import gleam/json
import gleam/list
import gleam/option.{None, Some}
import gleam/string
import gleeunit
import simplifile

pub fn main() -> Nil {
  gleeunit.main()
}

/// A tool-bearing turn digests to its verbs/targets/exit codes, not its output.
pub fn actions_summary_test() {
  let steps = [
    agent.StepPlan("I'll edit it"),
    agent.StepCall("EDIT", "README.md", "find/replace"),
    agent.StepExec("EDIT", 0, "edited README.md"),
    agent.StepCall("RUN", "git status", ""),
    agent.StepExec("RUN", 0, "M README.md"),
    agent.StepCheck(True, "OK"),
  ]
  let assert Some(summary) = router.actions_summary(steps)
  assert string.contains(summary, "EDIT README.md (exit 0)")
  assert string.contains(summary, "RUN git status (exit 0)")
  assert string.contains(summary, "CHECK passed")
}

/// A purely conversational turn (only prose) has no actions — no digest.
pub fn actions_summary_conversational_test() {
  assert router.actions_summary([agent.StepPlan("here's an overview")]) == None
}

/// `history_of` skips display-only tool entries and folds a `System` action
/// digest into the front of the assistant turn it precedes.
pub fn history_of_folds_digest_test() {
  let digest = "[Context — actions you performed this turn: EDIT README.md]"
  let tree =
    session.new("s1", "proj")
    |> session.append(history_entry("u1", None, session.User, "update the readme"))
    |> session.append(history_entry(
      "t1",
      Some("u1"),
      session.ToolResult,
      "{\"type\":\"call\"}",
    ))
    |> session.append(history_entry("sys1", Some("t1"), session.System, digest))
    |> session.append(history_entry(
      "a1",
      Some("sys1"),
      session.Assistant,
      "Done.",
    ))
  assert router.history_of(tree)
    == [#("user", "update the readme"), #("assistant", digest <> "\n\nDone.")]
}

fn history_entry(
  id: String,
  parent: option.Option(String),
  role: session.Role,
  content: String,
) -> session.Entry {
  session.Entry(
    id: id,
    parent_id: parent,
    role: role,
    content: content,
    snapshot_ref: None,
    label: None,
    timestamp: 0,
    grafted_from: None,
  )
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
    json.to_string(net_profile.build(
      [
        "https://api.foo.com/v1/**", "https://api.foo.com/v2/**",
        "bare.example.com",
      ],
      False,
      [],
      [],
      [],
      [],
      [],
    ))
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
    json.to_string(net_profile.build(
      [],
      True,
      [],
      [],
      [#("github_token", "GITHUB_TOKEN")],
      [],
      [],
    ))
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

/// Every base profile grants the git_config group (so git steps can read the
/// user's config). With `block`, the network section denies all outbound; the
/// allowlist form is reserved for the net-gate-on path.
pub fn net_profile_base_grants_test() {
  let blocked = json.to_string(net_profile.build([], True, [], [], [], [], []))
  assert string.contains(blocked, "\"git_config\"")
  assert string.contains(blocked, "\"block\":true")

  let open = json.to_string(net_profile.build([], False, [], [], [], [], []))
  assert string.contains(open, "\"git_config\"")
  assert string.contains(open, "allow_domain")

  // Session-enabled groups layer on top of the always-on git_config.
  let with_groups =
    json.to_string(net_profile.build([], True, ["user_caches_macos"], [], [], [], []))
  assert string.contains(with_groups, "\"git_config\"")
  assert string.contains(with_groups, "\"user_caches_macos\"")
}

/// A credential capability's paths become a `filesystem` section that both
/// grants read and bypasses any deny group covering them (so a path under
/// nono's locked `deny_credentials`, e.g. `~/.aws`, becomes readable).
pub fn net_profile_credential_filesystem_test() {
  let j = json.to_string(net_profile.build([], False, [], ["~/.aws"], [], [], []))
  assert string.contains(j, "\"filesystem\"")
  assert string.contains(j, "\"read\":[\"~/.aws\"]")
  assert string.contains(j, "\"bypass_protection\":[\"~/.aws\"]")
  // No filesystem section when no credential paths are granted.
  assert !string.contains(
    json.to_string(net_profile.build([], False, [], [], [], [], [])),
    "filesystem",
  )
}

/// The Seatbelt profile is allow-default reads minus the credential denylist
/// (~-expanded; absolute entries preserved) and deny-default writes except the
/// workspace + allowlist.
pub fn seatbelt_profile_test() {
  let p = seatbelt.build("/work/space", "/Users/x")
  assert string.contains(p, "(allow default)")
  assert string.contains(p, "(deny file-read*")
  assert string.contains(p, "(subpath \"/Users/x/.ssh\")")
  assert string.contains(p, "(subpath \"/Users/x/Library/Keychains\")")
  // absolute (non-~) denylist entry preserved verbatim
  assert string.contains(p, "(subpath \"/Library/Keychains\")")
  // write-confinement: deny by default, workspace allowed
  assert string.contains(p, "(deny file-write*)")
  assert string.contains(p, "(subpath \"/work/space\")")
}

/// `parse_env` reads KEY=VALUE lines, trims keys, and skips blanks/non-pairs.
pub fn providers_parse_env_test() {
  let kv = providers.parse_env("TOKEN=abc123\n\n  FOO =bar\nnotpair")
  assert dict.get(kv, "TOKEN") == Ok("abc123")
  assert dict.get(kv, "FOO") == Ok("bar")
  assert dict.size(kv) == 2
}

/// An egress provider runs prepare, sets the secret in bough's env (so nono
/// reads it outside the sandbox), and enables nono's managed `service` route —
/// no env var is forwarded into the sandbox.
pub fn providers_apply_egress_test() {
  let p =
    providers.Provider(
      name: "ghx",
      description: "",
      allow: ["github.com", "api.github.com"],
      reads: [],
      prepare: "",
      mode: providers.Egress,
      service: "github",
    )
  let app =
    providers.apply_list([p], fn(_cmd) { Ok("GITHUB_TOKEN=ghp_secret") }, fn(
      _k,
      _v,
    ) {
      Nil
    })
  assert app.services == ["github"]
  assert list.contains(app.allow, "api.github.com")
  assert app.env_allow == []
}

/// An aws (env) provider forwards the prepared KEY names into the sandbox via
/// allow_vars and enables no managed route.
pub fn providers_apply_env_test() {
  let app =
    providers.apply(
      ["aws"],
      fn(_cmd) { Ok("AWS_ACCESS_KEY_ID=AK\nAWS_SECRET_ACCESS_KEY=SK") },
      fn(_k, _v) { Nil },
    )
  assert app.services == []
  assert list.contains(app.env_allow, "AWS_ACCESS_KEY_ID")
  assert list.contains(app.env_allow, "AWS_SECRET_ACCESS_KEY")
  assert list.contains(app.allow, "sts.amazonaws.com")
}

/// A managed egress service renders nono's network.credentials; env-mode
/// forwarding renders environment.allow_vars.
pub fn net_profile_services_test() {
  let j =
    json.to_string(net_profile.build([], False, [], [], [], ["github"], []))
  assert string.contains(j, "\"credentials\":[\"github\"]")

  let e =
    json.to_string(net_profile.build([], False, [], [], [], [], [
      "AWS_ACCESS_KEY_ID",
    ]))
  assert string.contains(e, "\"allow_vars\":[\"AWS_ACCESS_KEY_ID\"]")
}

/// The capability suggester parses denied filesystem paths out of command
/// output (the part before the permission marker), and ignores clean output.
pub fn denied_paths_test() {
  let out =
    "mkdir: /Users/x/Library: Operation not permitted\n"
    <> "some other line\n"
    <> "cat: /etc/secret: Permission denied"
  assert engine.denied_paths(out) == ["/Users/x/Library", "/etc/secret"]
  assert engine.denied_paths("all good\nexit 0") == []
}

/// A round whose steps are ALL `collect` is a pure status poll — the harness
/// holds for subagents instead of re-prompting the model, which is what stops
/// the busy-wait. Any non-collect step (or an empty batch) makes it productive.
pub fn is_poll_round_test() {
  assert engine.is_poll_round([Collect("a", "id1")])
  assert engine.is_poll_round([Collect("a", "id1"), Collect("b", "id2")])
  assert engine.is_poll_round([]) == False
  assert engine.is_poll_round([Collect("a", "id1"), Code("c", "print(1)")])
    == False
  assert engine.is_poll_round([Code("c", "print(1)")]) == False
}

/// The suggester maps the worker's free-text reply to known toggleable group
/// names (case-insensitive), dropping unknowns and "none".
pub fn parse_suggested_test() {
  let catalog = [
    Group("user_caches_macos", "caches", "macos", False),
    Group("rust_runtime", "rust", "cross-platform", False),
  ]
  assert engine.parse_suggested("user_caches_macos, rust_runtime", catalog)
    == ["user_caches_macos", "rust_runtime"]
  assert engine.parse_suggested("USER_CACHES_MACOS\nbogus\nnone", catalog)
    == ["user_caches_macos"]
  assert engine.parse_suggested("none", catalog) == []
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
