//! Invariant (§2): the leader is an ORDINARY AGENT ROW with a plugin set mounted in its scope.
//! Nothing here gives it authority: it adopts unsorted mail, drafts requirements as claims, and
//! proposes structure — every one of which Andrey then accepts or rejects. What makes it "the
//! leader" is a `config.agent` field and four scoped registrations, which is why moving the set to
//! another agent is a patch and not a rewrite.
//!
//! Every registration is an EFFECT owned by THIS ROW's fiber and scoped to the target by SPEC
//! (P5-D11). Editing `leader.config.agent` is a material config diff, so the row reloads: the
//! section leaves the old agent's scope, the sink is replaced by the null sink, `ctx.leader` is
//! withdrawn, and `tool-leader` — which injects `leader` — unloads and reloads against the new
//! binding. No compile, no restart.

pub mod adopt;
pub mod draft;
pub mod error;
pub mod invariant;
pub mod persona;
pub mod timeline;
pub mod vocabulary;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_claims::OpenClaim;
use bough_plugin_ledger::{AgentName, StepId};

pub use adopt::{AdoptReport, AdoptRequest};
pub use draft::DraftRequest;
pub use error::LeaderError;
pub use timeline::{TimelineEntry, TimelineQuery, TimelineRow};
pub use vocabulary::{TimelineEntryBody, OWNER};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "leader";

/// The `leader` service key.
pub struct Leader;

impl ServiceKey for Leader {
    type Value = LeaderHandle;
    const NAME: &'static str = "leader";
}

/// The concrete handle newtype the key's value is (Decision D5).
#[derive(Clone)]
pub struct LeaderHandle(pub Arc<LeaderInner>);

/// The seam's bound dependencies and the one name that defines the set.
pub struct LeaderInner {
    #[allow(dead_code)]
    target: AgentName,
    #[allow(dead_code)]
    ledger: bough_plugin_ledger::LedgerHandle,
    #[allow(dead_code)]
    mail: bough_plugin_mail_router::MailHandle,
    #[allow(dead_code)]
    claims: bough_plugin_claims::ClaimsHandle,
    #[allow(dead_code)]
    graph: bough_plugin_graph_ops::GraphHandle,
    #[allow(dead_code)]
    cfg: Arc<LeaderConfig>,
}

impl LeaderHandle {
    /// The agent this set is mounted for. `tool-leader` reads it; nothing else needs it (P5-D10).
    pub fn target(&self) -> &AgentName {
        &self.0.target
    }

    /// Unsorted adoption (§2): read the queue, route each item to a lane, or hold it.
    pub async fn adopt(&self, _req: AdoptRequest) -> Result<AdoptReport, LeaderError> {
        todo!("WP-5: read ctx.mail.unsorted(), adopt the placements, hold the rest")
    }

    /// Requirement drafting from Andrey's words (§2): a claim, never a pin. Acceptance is his.
    pub async fn draft_requirement(&self, _req: DraftRequest) -> Result<OpenClaim, LeaderError> {
        todo!("WP-5: propose a ClaimKind::Requirement citing Andrey's words")
    }

    /// Cross-agent timeline DATA (§17: the surface is Phase 8).
    pub async fn note_timeline(&self, _e: TimelineEntry) -> Result<StepId, LeaderError> {
        todo!("WP-5: append a cited timeline/entry")
    }

    /// Read the timeline back.
    pub async fn timeline(&self, _q: &TimelineQuery) -> Result<Vec<TimelineRow>, LeaderError> {
        todo!("WP-5: query timeline/entry steps")
    }
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LeaderConfig {
    /// THE field this phase's SWAP test edits. The one agent whose scope holds the set.
    pub agent: String,
    /// The persona section's text, contributed at `Slot::Identity` / `Place::After`.
    pub persona: String,
    /// How many unsorted items `adopt_unsorted` may take at once.
    pub adopt_batch: usize,
    /// Attribute reconsolidation passes to the leader (§8) when `reconsolidation` is bound.
    pub attribute_reconsolidation: bool,
}

/// The `leader` row.
pub struct LeaderPlugin;

#[async_trait::async_trait]
impl Plugin for LeaderPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LeaderConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "ledger", "mail", "graph", "claims", "projection"])
            .union(&Inject::optional(["reconsolidation"]))
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!(
            "WP-5: declare timeline/entry, register the persona section and the unsorted sink, \
               provide `leader`"
        )
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::adoption_names_its_unrouted_step()]
    }
}

bough_kernel::register_plugin!(LeaderPlugin);
