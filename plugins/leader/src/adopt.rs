//! Invariant (§2): adoption ROUTES, it never decides. The leader reads the durable unsorted queue
//! and either places an item with a lane — appending `mail/adopted` naming the `mail/unrouted`
//! step it consumes, so the adoption is attributable — or HOLDS it. Holding is a legitimate
//! outcome: an item the leader cannot place stays in the queue rather than being forced into the
//! nearest lane.

use bough_plugin_ledger::{AgentName, StepId};
use chrono::{DateTime, Utc};

/// One adoption pass.
#[derive(Clone, Debug)]
pub struct AdoptRequest {
    /// `None` ⇒ up to `adopt_batch` items, oldest first.
    pub steps: Option<Vec<StepId>>,
    /// Where each item goes. An item with no entry is HELD.
    pub placements: Vec<(StepId, AgentName)>,
    pub at: DateTime<Utc>,
}

/// What one adoption pass did.
#[derive(Clone, Debug, PartialEq)]
pub struct AdoptReport {
    pub adopted: Vec<(StepId, AgentName)>,
    /// Items the leader could not place. They stay in the queue.
    pub held: Vec<StepId>,
}
