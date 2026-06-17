//// Typed bridge to nono (https://nono.sh). These are pure data types describing
//// the sandbox contract; `bough_server` turns them into `nono` CLI invocations
//// and parses its audit/proxy output back into these shapes (SPEC.md §6, §7).

import gleam/option.{type Option}

pub type NetDecision {
  Allow
  Deny
}

pub fn decision_from_string(s: String) -> Result(NetDecision, Nil) {
  case s {
    "allow" -> Ok(Allow)
    "deny" -> Ok(Deny)
    _ -> Error(Nil)
  }
}

/// A single network rule. The default posture is deny; `allow_domains` in a
/// `Profile` are the exceptions.
pub type NetRule {
  NetRule(host: String, decision: NetDecision)
}

/// The sandbox capability profile bough emits per session.
pub type Profile {
  Profile(
    workspace: String,
    allow_domains: List(String),
    block_net: Bool,
    rollback: Bool,
  )
}

/// One observed egress event from nono's proxy audit log, parsed for the
/// network side pane (SPEC.md §7). Mirrors a nono `network_events` entry.
pub type AuditEvent {
  AuditEvent(
    host: String,
    port: Int,
    method: Option(String),
    path: Option(String),
    decision: NetDecision,
    reason: Option(String),
    timestamp: Int,
  )
}

/// A nono rollback snapshot reference recorded on a session node (SPEC.md §4.1).
/// `session_id` is nono's rollback/audit session id; `reference` is the
/// snapshot number within it.
pub type Snapshot {
  Snapshot(session_id: String, reference: String, created: Int)
}

/// Default-deny profile allowing only the workspace and the given domains.
pub fn default_profile(workspace: String, allow: List(String)) -> Profile {
  Profile(
    workspace: workspace,
    allow_domains: allow,
    block_net: False,
    rollback: True,
  )
}
