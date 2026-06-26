import bough_core/artifact.{Code, Collect}
import bough_core/session
import bough_server/agent
import bough_server/control
import bough_server/engine
import bough_server/router
import bough_server/json_value
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

pub fn json_value_round_trip_test() {
  // Covers the tool-use round-trip: arbitrary JSON must re-encode unchanged.
  let src =
    "{\"a\":\"x\",\"n\":1,\"b\":true,\"z\":null,\"arr\":[1,\"two\",false]}"
  let assert Ok(value) = json.parse(src, json_value.decoder())
  let assert Ok(reparsed) =
    json.parse(json.to_string(json_value.to_json(value)), json_value.decoder())
  assert reparsed == value
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

/// The Seatbelt profile is allow-default reads minus the credential denylist
/// (~-expanded; absolute entries preserved) and deny-default writes except the
/// workspace + allowlist.
pub fn seatbelt_profile_test() {
  let p = seatbelt.build("/work/space", "/Users/x", None)
  assert string.contains(p, "(allow default)")
  assert string.contains(p, "(deny file-read*")
  assert string.contains(p, "(subpath \"/Users/x/.ssh\")")
  assert string.contains(p, "(subpath \"/Users/x/Library/Keychains\")")
  // absolute (non-~) denylist entry preserved verbatim
  assert string.contains(p, "(subpath \"/Library/Keychains\")")
  // write-confinement: deny by default, workspace allowed
  assert string.contains(p, "(deny file-write*)")
  assert string.contains(p, "(subpath \"/work/space\")")
  // no proxy port -> network left open
  assert !string.contains(p, "(deny network*)")

  // with a proxy port, egress is locked to that loopback port
  let locked = seatbelt.build("/work/space", "/Users/x", Some(8080))
  assert string.contains(locked, "(deny network*)")
  assert string.contains(locked, "(remote ip \"localhost:8080\")")
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

