//! Invariant: the bytes a snapshot holds are `Assembled::to_text()` and nothing else — the same
//! function of the same `Assembled` that `agent-loop`'s `request::build` puts in
//! `LlmRequest::system`. Nothing in this module re-spells the surface (§0.2, D-C1).

use bough_plugin_ledger::{AgentName, LedgerHandle, Seq};
use bough_plugin_projection::{Assembled, Flag, ProjectionHandle, SectionId};
use chrono::{DateTime, Utc};
use std::collections::BTreeSet;

use crate::error::PreviewError;

/// WHICH ledger high-water the preview assembles at. The pane's whole honesty question (D-C1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreviewAt {
    /// `as_of = ledger.head_seq(traj)`: what the agent would see if it woke this instant — before
    /// the wake writes its own `wake/start`, its mail deliveries and its `step/start`.
    Head,
    /// A named high-water: exactly the value a past wake's `request/header.as_of` carries. The
    /// mode V1 asserts byte-exactness in.
    Seq(Seq),
}

/// One taken preview. `text` is THE byte-exact surface.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub agent: AgentName,
    pub at: PreviewAt,
    pub as_of: Seq,
    /// `Assembled::to_text()`, and nothing else.
    pub text: String,
    pub tokens: usize,
    pub budget: usize,
    pub flags: BTreeSet<Flag>,
    /// `(section id, tokens)`, in render order.
    pub sections: Vec<(SectionId, usize)>,
    /// sha256 hex of `text`; equals `request/header.projection_digest` for the same `as_of`.
    pub digest: String,
    pub taken_at: DateTime<Utc>,
}

/// Take a preview. The ONLY call in this crate that reaches the seam; everything else is pure.
///
/// Resolves the agent's trajectory through the ledger's mutable `agents` row (never a `?? default`
/// trajectory), reads `head_seq` for [`PreviewAt::Head`], and calls `ProjectionHandle::assemble`
/// with `wake: None` and `budget: None` — the same call `agent-loop` makes, with the same
/// defaults, so the bytes are the loop's by construction.
///
/// WP-1.
pub async fn snapshot(
    projection: &ProjectionHandle,
    ledger: &LedgerHandle,
    agent: &AgentName,
    at: PreviewAt,
    now: DateTime<Utc>,
) -> Result<Snapshot, PreviewError> {
    let _ = (projection, ledger, agent, at, now);
    todo!("WP-1: resolve the trajectory, resolve as_of, assemble, hash")
}

/// PURE: the system prefix a request built from `a` carries.
///
/// One line, and it exists so the claim "the pane and the loop spell this the same way" is a call
/// and not a comment.
pub fn system_prefix(a: &Assembled) -> String {
    a.to_text()
}

/// PURE: sha256 hex. The same spelling as `agent_loop::request::digest`.
///
/// WP-1.
pub fn digest(text: &str) -> String {
    let _ = text;
    todo!("WP-1: sha256 hex of the text")
}
