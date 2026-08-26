//! Invariant: a draft is a STEP and a pane row, and NOTHING ELSE HAPPENS. There is no code path
//! from this crate to a network, to `ctx.actions`, or to any outward surface — the absence is the
//! feature, and `src/invariant.rs` checks the ledger for it rather than trusting the reading.
//!
//! P6-D4: a draft is a THOUGHT, not evidence. It is the agent's own composition; making it
//! evidence would force a citation the agent may not have and would let a draft launder an
//! assertion into the record. Its refs ride on the body so the pane and Phase 5's router can index
//! it.

pub mod invariant;
pub mod tool;
pub mod vocabulary;

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError, ServiceKey};
use bough_plugin_ledger::{AgentName, LedgerHandle, Ref, StepId, WakeId};
use chrono::{DateTime, Utc};

pub use vocabulary::{DraftMessage, DraftTicket, DRAFT_MESSAGE, DRAFT_TICKET};

/// The catalog name of the Definition row.
pub const PLUGIN_NAME: &str = "drafts";
/// The catalog name of the tool row.
pub const TOOL_PLUGIN_NAME: &str = "tool-drafts";

/// The `drafts` service key.
pub struct Drafts;

impl ServiceKey for Drafts {
    type Value = DraftsHandle;
    const NAME: &'static str = "drafts";
}

bough_util::brand_id!(
    /// One draft.
    pub struct DraftId;
);

/// A branded id is a plain string in a body schema. `brand_id!` lives in `bough-util`, which has no
/// `schemars` dependency (§0.1), so the impl is written here — the `plugins/ledger/src/id.rs` shape.
impl schemars::JsonSchema for DraftId {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Borrowed("DraftId")
    }
    fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string" })
    }
}

/// Which of the two things a draft is.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum DraftKind {
    Message,
    Ticket,
}

/// A draft to write.
#[derive(Clone, Debug, PartialEq)]
pub struct NewDraft {
    pub kind: DraftKind,
    pub agent: AgentName,
    pub wake: WakeId,
    /// Where it WOULD go: `"slack:#eng"`, `"linear:TEAM"`, `"email:someone"`. Free text on
    /// purpose: the harness never resolves it, because it never sends it.
    pub audience: String,
    pub subject: String,
    pub body: String,
    pub refs: BTreeSet<Ref>,
    pub at: DateTime<Utc>,
}

/// A draft as read back.
#[derive(Clone, Debug, PartialEq)]
pub struct DraftRow {
    pub id: DraftId,
    pub step: StepId,
    pub kind: DraftKind,
    pub agent: AgentName,
    pub audience: String,
    pub subject: String,
    pub body: String,
    pub refs: Vec<Ref>,
    pub at: DateTime<Utc>,
}

/// What to list.
#[derive(Clone, Debug, Default)]
pub struct DraftQuery {
    pub agents: Vec<AgentName>,
    pub kind: Option<DraftKind>,
    pub limit: Option<usize>,
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct DraftsHandle(pub Arc<DraftsInner>);

/// The seam's state: the ledger it appends into, and the retention the pane reads with.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct DraftsInner {
    ledger: LedgerHandle,
    retain: usize,
}

impl DraftsHandle {
    /// A handle over one ledger.
    pub fn new(ledger: LedgerHandle, retain: usize) -> DraftsHandle {
        DraftsHandle(Arc::new(DraftsInner { ledger, retain }))
    }

    /// Append a `draft/message` or `draft/ticket` step and return it. Nothing else happens. WP-4.
    pub async fn draft(&self, d: NewDraft) -> Result<DraftRow, DraftError> {
        let _ = d;
        todo!("WP-4")
    }

    /// Read drafts back out of the ledger. WP-4.
    pub async fn list(&self, q: &DraftQuery) -> Result<Vec<DraftRow>, DraftError> {
        let _ = q;
        todo!("WP-4")
    }
}

/// What a draft refuses.
#[derive(Debug, thiserror::Error)]
pub enum DraftError {
    #[error("a draft needs an audience: say where it would go (`slack:#eng`, `linear:TEAM`, …)")]
    NoAudience,
    #[error("a draft needs a body")]
    Empty,
    #[error("`{0}` is not a live agent")]
    UnknownAgent(AgentName),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
}

/// The Definition row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftsConfig {
    /// How many drafts the pane and `list()` read back by default.
    pub retain: usize,
}

/// The Definition row.
pub struct DraftsPlugin;

#[async_trait::async_trait]
impl Plugin for DraftsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = DraftsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-4: `retain > 0`")
    }

    /// Declare the two step types as an effect, then provide `drafts`. WP-4.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(DraftsPlugin);
bough_kernel::register_plugin!(tool::DraftToolsPlugin);
