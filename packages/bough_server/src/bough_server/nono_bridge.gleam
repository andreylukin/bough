//// Bridge to the nono CLI. Since nono has no BEAM SDK, bough drives the CLI and
//// parses its session registry, proxy audit log, and rollback metadata back
//// into `bough_core/nono` types (SPEC.md §6).
////
//// These are typed stubs; the implementations shell out via a port/erlexec.

import bough_core/nono.{type AuditEvent, type Profile, type Snapshot}

/// Launch a sandboxed agent: `nono run [--detached] --rollback <profile> -- <cmd>`.
/// Returns the nono session id.
pub fn launch(_profile: Profile, _command: List(String)) -> Result(String, String) {
  Error("nono_bridge.launch: not implemented")
}

/// Tail the proxy audit log for a session, feeding the network side pane.
pub fn audit_events(_session_id: String) -> Result(List(AuditEvent), String) {
  Error("nono_bridge.audit_events: not implemented")
}

/// Capture a rollback snapshot for the current filesystem state.
pub fn snapshot(_session_id: String) -> Result(Snapshot, String) {
  Error("nono_bridge.snapshot: not implemented")
}

/// Restore a snapshot before continuing from a forked node (SPEC.md §4.1).
pub fn restore(_snapshot: Snapshot) -> Result(Nil, String) {
  Error("nono_bridge.restore: not implemented")
}

/// `nono stop <id>` — terminate a session cleanly.
pub fn stop(_session_id: String) -> Result(Nil, String) {
  Error("nono_bridge.stop: not implemented")
}
