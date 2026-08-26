//! Invariant: reconciliation NEVER CALLS A WRITE PATH. For every pending row it performs one
//! read — "is my marker in the world?" — through the same Provider that would have executed it,
//! and then either marks the row done with the located artifact or writes a DRAFT describing the
//! unfinished intent and leaves the row `Intent`. A pending row is never re-executed (§7).
//!
//! P6-D12: [`ArtifactLookup`] is a SECOND trait registered here, because `plugins/actions` is
//! off-limits in this track and `ActionProvider` cannot grow a method. Merge note 2 folds it in.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{
    ConfigError, Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey,
};
use bough_plugin_actions::{ActionArtifact, ActionError, ActionKind, ActionsHandle};
use bough_plugin_ledger::ActionId;
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "actions-reconcile";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReconcileConfig {
    pub at_boot: bool,
    /// Whose lane an unfinished intent is surfaced on, as a draft.
    pub surface_to: String,
}

/// What a Provider must answer for reconciliation to be a lookup and not a guess.
#[async_trait::async_trait]
pub trait ArtifactLookup: Send + Sync + 'static {
    fn kinds(&self) -> Vec<ActionKind>;
    /// `Ok(Some(artifact))` ⇒ the marker was found in the world. A READ, always.
    async fn find_marker(
        &self,
        kind: ActionKind,
        canonical_target: &str,
        marker: &str,
    ) -> Result<Option<ActionArtifact>, ActionError>;
}

/// The `action_lookup` service key: the registry this row owns, so `actions-github` and
/// `actions-linear` can register their lookup halves without touching `plugins/actions`.
pub struct ActionLookup;

impl ServiceKey for ActionLookup {
    type Value = LookupRegistry;
    const NAME: &'static str = "action_lookup";
}

/// Registered lookups, each paired with the id its disposer removes.
type Registered = Vec<(u64, Arc<dyn ArtifactLookup>)>;

/// The registry: kind → lookup, with registration as an effect.
#[derive(Clone, Default)]
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct LookupRegistry(Arc<parking_lot::Mutex<Registered>>);

impl LookupRegistry {
    /// An empty registry.
    pub fn new() -> LookupRegistry {
        LookupRegistry::default()
    }
    /// Register a lookup. An EFFECT: the disposer removes exactly this registration. WP-3.
    pub async fn register(
        &self,
        ctx: &Context,
        lookup: Arc<dyn ArtifactLookup>,
    ) -> Result<EffectHandle, PluginError> {
        let _ = (ctx, lookup);
        todo!("WP-3")
    }
    /// The lookup that claims `kind`, if any. WP-3.
    pub fn for_kind(&self, kind: ActionKind) -> Option<Arc<dyn ArtifactLookup>> {
        let _ = kind;
        todo!("WP-3")
    }
}

/// What one reconciliation pass did.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReconcileReport {
    pub marked_done: Vec<ActionId>,
    pub surfaced: Vec<ActionId>,
    /// Pending rows whose kind no registered lookup claims. Reported, never guessed at.
    pub unknown_kind: Vec<ActionId>,
}

/// The reconciler.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct Reconciler {
    cfg: Arc<ReconcileConfig>,
    actions: ActionsHandle,
    lookups: LookupRegistry,
    drafts: bough_plugin_drafts::DraftsHandle,
    agents: bough_plugin_agents::AgentsHandle,
}

impl Reconciler {
    /// One pass over `ActionsHandle::pending()`. The clock is INJECTED. WP-3.
    pub async fn reconcile(&self, now: DateTime<Utc>) -> Result<ReconcileReport, ActionError> {
        let _ = now;
        todo!(
            "WP-3: found ⇒ action_done(Done); absent ⇒ a draft on `surface_to` and LEAVE it Intent"
        )
    }
}

/// The row.
pub struct ReconcilePlugin;

#[async_trait::async_trait]
impl Plugin for ReconcilePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ReconcileConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["actions", "drafts", "agents"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-3: `surface_to` must name an agent")
    }

    /// Provide the lookup registry FIRST (the two action Providers inject it), then run one pass
    /// when `at_boot`. WP-3.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-3")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ReconcilePlugin);
