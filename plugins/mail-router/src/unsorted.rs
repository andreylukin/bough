//! Invariant (P5-D4): the unsorted queue is a REAL trajectory and the leader is a SINK on it, not
//! its owner. A tree may boot with no leader — headless, and the moment before the `leader` row
//! activates — and mail must be neither dropped nor refused then. So the queue is durable and
//! leaderless, and a sink that arrives later adopts the backlog.

use bough_plugin_agents::InboxReceipt;
use bough_plugin_ledger::{AgentName, StepId};
use chrono::{DateTime, Utc};

/// Who receives unsorted mail as LIVE mail. An effect: the `leader` row installs it in its own
/// fiber, so moving the leader set moves the sink with it (the SWAP).
#[async_trait::async_trait]
pub trait UnsortedSink: Send + Sync + 'static {
    /// The agent unsorted mail is delivered to.
    fn agent(&self) -> AgentName;
}

/// The sink that is mounted when no leader is: it names nobody, and the queue simply keeps its
/// items until a real sink arrives.
pub struct NullSink;

#[async_trait::async_trait]
impl UnsortedSink for NullSink {
    fn agent(&self) -> AgentName {
        todo!("WP-1: the null sink has no agent; `route` must check for it before delivering")
    }
}

/// What one adoption did.
#[derive(Clone, Debug, PartialEq)]
pub struct Adoption {
    pub unrouted: StepId,
    pub to: AgentName,
    pub receipt: InboxReceipt,
    pub at: DateTime<Utc>,
}
