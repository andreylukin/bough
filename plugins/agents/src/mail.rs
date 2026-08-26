//! Invariant (§2): EVERY inbox mutation is a durable `inbox/spliced` step keyed by the message
//! id. The live inbox is a cache of that fold (P2-D8): `Inbox::rebuild` is the same function
//! resume and crash repair use, so the two copies cannot drift.

use std::collections::BTreeSet;

use bough_plugin_ledger::{Cite, Ref, Seq, Step, StepId};
use chrono::{DateTime, Utc};

use crate::error::AgentError;
use crate::ids::{AgentId, MessageId, WorkerId};

pub use bough_plugin_ledger::vocabulary::MailClass;
use bough_plugin_ledger::AgentName;

/// Which queue a message lands in (§2).
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Delivered at the START of the next wake.
    NextWake,
    /// Delivered at the next STEP boundary of the running wake.
    NextStep,
}

/// Who sent a message. `Andrey` is the one sender that changes the wake's class (§5).
#[derive(Clone, Debug, PartialEq)]
pub enum Sender {
    Andrey,
    Agent(AgentName),
    Worker(WorkerId),
    /// A collector row (Phase 6); named here so the vocabulary is complete.
    Collector(String),
    /// The harness itself: crash repair, a schedule firing, a bound being hit.
    System(&'static str),
}

/// One piece of mail.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: MessageId,
    pub from: Sender,
    /// The ledger's two urgencies (§5).
    pub class: MailClass,
    pub text: String,
    pub subject: String,
    pub cites: Vec<Cite>,
    pub refs: BTreeSet<Ref>,
    /// Set when this is DELIVERED mail with a `mail/delivered` step. Consumption is per
    /// (agent, seq) and applies to delivered mail only (§5).
    pub mail_seq: Option<Seq>,
    pub at: DateTime<Utc>,
}

impl Message {
    /// The one predicate §5's "an Andrey message ALWAYS gets a fresh sol answer wake" turns on.
    pub fn is_andrey(&self) -> bool {
        matches!(self.from, Sender::Andrey)
    }
}

/// What an inbox mutation produced: the durable step, and where the message went.
#[derive(Clone, Debug, PartialEq)]
pub struct InboxReceipt {
    pub message: MessageId,
    pub agent: AgentId,
    pub target: Target,
    /// Whether the sender asked for a wake. The driver decides what to do with it.
    pub wake: bool,
    pub step: StepId,
    pub seq: Seq,
}

/// A message the driver has claimed for a wake, with the claim's durable step.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedMessage {
    pub message: Message,
    pub target: Target,
    pub claim_step: StepId,
}

/// One agent's two queues.
pub struct Inbox {
    /// WP-2 fills this in. Named so the shape is visible in the scaffold.
    _queues: parking_lot::Mutex<Vec<(Message, Target)>>,
}

impl Inbox {
    /// An empty inbox. WP-2.
    pub fn new() -> Inbox {
        Inbox {
            _queues: parking_lot::Mutex::new(Vec::new()),
        }
    }

    /// Insert one message, appending its `inbox/spliced { op: insert }` step first.
    ///
    /// WP-2.
    pub async fn insert(&self, _msg: Message, _target: Target) -> Result<InboxReceipt, AgentError> {
        todo!("WP-2: durable splice, then the live cache")
    }

    /// Pending messages for one queue, oldest first. WP-2.
    pub fn pending(&self, _target: Target) -> Vec<Message> {
        todo!("WP-2: read the live cache")
    }

    /// Whether that queue has anything. WP-2.
    pub fn has(&self, _target: Target) -> bool {
        todo!("WP-2: read the live cache")
    }

    /// Total pending across both queues. WP-2.
    pub fn len(&self) -> usize {
        todo!("WP-2: read the live cache")
    }

    /// Whether both queues are empty. WP-2.
    pub fn is_empty(&self) -> bool {
        todo!("WP-2: read the live cache")
    }

    /// The pure fold over `inbox/spliced` steps: insert minus claim minus discard. Used at
    /// resume and by crash repair, so the live inbox and the ledger can never disagree (P2-D8).
    ///
    /// WP-2.
    pub fn rebuild(_steps: &[Step]) -> Vec<(Message, Target)> {
        todo!("WP-2: the fold, pure over the steps")
    }
}

impl Default for Inbox {
    fn default() -> Self {
        Inbox::new()
    }
}
