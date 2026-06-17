//// TUI application model. Kept behind a thin module so the rendering library
//// can be swapped (SPEC.md §9).

import bough_core/nono.{type AuditEvent}
import bough_core/session.{type SessionTree}
import gleam/option.{type Option}

/// Which overlay, if any, is currently shown over the chat pane.
pub type Overlay {
  NoOverlay
  /// The session tree navigator (`/tree`).
  TreeOverlay
}

pub type Model {
  Model(
    session: Option(SessionTree),
    /// Live egress feed for the network side pane (SPEC.md §7).
    net_feed: List(AuditEvent),
    input: String,
    overlay: Overlay,
  )
}

pub fn init() -> Model {
  Model(
    session: option.None,
    net_feed: [],
    input: "",
    overlay: NoOverlay,
  )
}
