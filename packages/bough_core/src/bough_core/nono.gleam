//// Typed bridge to nono (https://nono.sh). These are pure data types describing
//// the sandbox contract; `bough_server` turns them into `nono` CLI invocations
//// and parses its audit/proxy output back into these shapes (SPEC.md §6, §7).

pub type NetDecision {
  Allow
  Deny
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
/// network side pane (SPEC.md §7).
pub type AuditEvent {
  AuditEvent(
    host: String,
    method: String,
    path: String,
    decision: NetDecision,
    timestamp: Int,
  )
}

/// A nono rollback snapshot reference recorded on a session node (SPEC.md §4.1).
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
