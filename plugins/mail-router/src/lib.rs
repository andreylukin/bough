//! Invariant: this crate CHOOSES RECIPIENTS and nothing else. It never re-implements delivery:
//! `route` calls [`bough_plugin_agents::Agent::deliver`] once per recipient, which appends
//! `mail/delivered` FIRST and then splices the message carrying that step's seq (P3-D15) — and
//! that is what makes per-agent consumption free rather than a second bookkeeping scheme.
//!
//! The fan-out rule (§3) is EVERY matching agent, not the best one; zero matches land in the
//! durable unsorted queue (P5-D4), which exists whether or not a leader does.

pub mod envelope;
pub mod error;
pub mod invariant;
pub mod link;
pub mod matching;
pub mod question;
pub mod unsorted;
pub mod vocabulary;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{
    Context, EffectHandle, EmitEvent, Inject, InvariantSpec, Plugin, PluginError, ServiceKey,
    WaterfallEvent,
};
use bough_plugin_agents::InboxReceipt;
use bough_plugin_ledger::{AgentName, Ref, Step, StepId};
use chrono::{DateTime, Utc};

pub use envelope::{Envelope, LinkReport, Question, RouteReport};
pub use error::MailError;
pub use link::{linked, unlinked};
pub use matching::{recipients, wake_classes_of, CLASS_NAMESPACE};
pub use question::{ask_ref, ASK_CLASS_REF};
pub use unsorted::{Adoption, NullSink, UnsortedSink};
pub use vocabulary::{AgentRouting, LeaderQuestion, MailAdopted, MailUnrouted, OWNER};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "mail-router";

/// The `mail` service key.
pub struct Mail;

impl ServiceKey for Mail {
    type Value = MailHandle;
    const NAME: &'static str = "mail";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct MailHandle(pub Arc<MailInner>);

/// The seam's live state: the bound ledger and agents seams, the config, and the sink slot.
pub struct MailInner {
    #[allow(dead_code)]
    ledger: bough_plugin_ledger::LedgerHandle,
    #[allow(dead_code)]
    agents: bough_plugin_agents::AgentsHandle,
    #[allow(dead_code)]
    cfg: Arc<MailConfig>,
    /// P5-D4: leaderless by default. [`NullSink`] until a `leader` row installs its own.
    #[allow(dead_code)]
    sink: parking_lot::Mutex<Option<Arc<dyn UnsortedSink>>>,
}

impl MailHandle {
    /// A seam with no sink mounted.
    pub fn new(
        _ledger: bough_plugin_ledger::LedgerHandle,
        _agents: bough_plugin_agents::AgentsHandle,
        _cfg: Arc<MailConfig>,
    ) -> MailHandle {
        todo!("WP-1: construct the inner with a null sink")
    }

    /// Fan out. Seeds a [`RouteDecision`] from the pure matcher, dispatches the `mail/route`
    /// waterfall (P5-D5), then delivers once per surviving recipient. Appends nothing when
    /// `matched` is empty except the unsorted step.
    pub async fn route(&self, _env: Envelope) -> Result<RouteReport, MailError> {
        todo!("WP-1: seed, dispatch mail/route, deliver per recipient, else queue unsorted")
    }

    /// Add routing refs to an agent's row and append `agent/routing`. No backfill, by
    /// construction: this method never queries for history.
    pub async fn link_ref(
        &self,
        _agent: &AgentName,
        _refs: BTreeSet<Ref>,
        _at: DateTime<Utc>,
    ) -> Result<LinkReport, MailError> {
        todo!("WP-1: put_agent with the union, append agent/routing, report backfilled: 0")
    }

    /// Remove routing refs from an agent's row and append `agent/routing`.
    pub async fn unlink_ref(
        &self,
        _agent: &AgentName,
        _refs: BTreeSet<Ref>,
        _at: DateTime<Utc>,
    ) -> Result<LinkReport, MailError> {
        todo!("WP-1: put_agent with the difference, append agent/routing")
    }

    /// §4's "ambiguous routing becomes a leader question, never a guess". Appends
    /// `leader/question` to the unsorted trajectory and routes it at `MailClass::Wake` with the
    /// `class:ask` ref, so it reactivates a dormant leader.
    pub async fn ask_leader(&self, _q: Question) -> Result<StepId, MailError> {
        todo!("WP-1: append leader/question, then route the wake-class envelope for it")
    }

    /// The unsorted queue, oldest first: what the leader's `adopt_unsorted` reads.
    pub async fn unsorted(&self, _limit: usize) -> Result<Vec<Step>, MailError> {
        todo!("WP-1: query the unsorted trajectory for mail/unrouted, seq-ascending")
    }

    /// Mark unsorted items adopted by an agent: appends `mail/adopted` and re-routes them to it.
    pub async fn adopt(
        &self,
        _to: &AgentName,
        _steps: &[StepId],
        _at: DateTime<Utc>,
    ) -> Result<Vec<InboxReceipt>, MailError> {
        todo!("WP-1: append mail/adopted per step and deliver each to `to`")
    }

    /// Install the unsorted sink. An EFFECT: the `leader` row installs it in its own fiber, so
    /// moving the leader set moves the sink with it, and unloading restores the null sink.
    pub async fn unsorted_sink(
        &self,
        _ctx: &Context,
        _sink: Arc<dyn UnsortedSink>,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-1: set the slot, defer restoring the null sink")
    }
}

// ---- events ------------------------------------------------------------------------------

/// The value the `mail/route` waterfall carries.
#[derive(Clone)]
pub struct RouteDecision {
    pub env: Arc<Envelope>,
    pub to: Vec<AgentName>,
}

/// `mail/route` — WATERFALL over the routing DECISION: the extension point §0.2 names for the
/// mail domain. A later row (a ward, a collector policy) may add or remove recipients.
///
/// P5-D5: the crate's own matcher is NOT a listener. It SEEDS the decision before dispatch, so a
/// policy listener that deliberately skips `next()` short-circuits to a decision that already
/// exists rather than to an empty one.
pub struct MailRoute;
impl WaterfallEvent for MailRoute {
    const NAME: &'static str = "mail/route";
    type Value = RouteDecision;
}

/// `mail/routed` — EMIT, post-delivery. The TUI's toast and the invariant read it.
pub struct MailRouted;
impl EmitEvent for MailRouted {
    const NAME: &'static str = "mail/routed";
    type Payload = RouteReport;
}

// ---- the row -----------------------------------------------------------------------------

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MailConfig {
    /// The durable trajectory zero-match mail lands on (P5-D4).
    pub unsorted_traj: String,
    /// How many unsorted items one `unsorted()` read returns at most.
    pub unsorted_limit: usize,
    /// Defaults `true`, and NOT a dormancy switch: §5 says mail QUEUES for a dormant agent, so
    /// delivery happens and the WAKE is what dormancy suppresses.
    pub deliver_to_dormant: bool,
}

/// The `mail` row.
pub struct MailRouterPlugin;

#[async_trait::async_trait]
impl Plugin for MailRouterPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = MailConfig;

    fn inject() -> Inject {
        Inject::required(["ledger", "agents"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: declare the step types, provide `mail`, register the invariants")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![
            invariant::unrouted_matched_nobody(),
            invariant::one_delivery_per_recipient(),
        ]
    }
}

bough_kernel::register_plugin!(MailRouterPlugin);
