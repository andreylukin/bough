//// Egress-event types for the network side pane (SPEC.md §7): an observed
//// outbound request (allow/deny) the sandbox's proxy reports, parsed into these
//// shapes for the run's network feed.

import gleam/option.{type Option}

pub type NetDecision {
  Allow
  Deny
}

/// One observed egress event from the proxy audit log, for the network feed.
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
