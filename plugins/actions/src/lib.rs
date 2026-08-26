//! Invariant: this crate is the actions SERVICE DEFINITION (§7). It owns the `actions` key, the
//! four kinds, the idempotency journal and the `actions/execute` waterfall — and NO PROVIDER.
//! Phase 2 registers none (§17 Phase 6), so every kind is refused with `NoProvider` naming it:
//! the refusal is what the model meets, and the journal exists before the capability does.
//!
//! P2-D1: it owns live state (the provider registry and the journal handle), so it IS a catalog
//! row and provides its own key.

pub mod error;
pub mod invariant;
pub mod journal;
pub mod kind;

use std::sync::Arc;

use bough_kernel::{
    Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey, WaterfallEvent,
};

pub use error::ActionError;
pub use journal::{
    idem_key, marker_for, ActionArtifact, ActionRequest, ExecuteRequest, PendingAction,
};
pub use kind::{ActionKind, ActionTarget};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "actions";

/// The `actions` service key.
pub struct Actions;

impl ServiceKey for Actions {
    type Value = ActionsHandle;
    const NAME: &'static str = "actions";
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct ActionsHandle(pub Arc<ActionsInner>);

/// The seam's live state: which kinds have a Provider.
pub struct ActionsInner {
    /// WP-7 fills this in. Empty in Phase 2, on purpose.
    _providers: parking_lot::Mutex<Vec<Arc<dyn ActionProvider>>>,
}

/// What an action Provider does. Phase 6 writes them.
#[async_trait::async_trait]
pub trait ActionProvider: Send + Sync + 'static {
    fn kinds(&self) -> Vec<ActionKind>;
    /// The Provider embeds `req.marker` in the artifact itself (PR body, commit trailer, comment
    /// suffix) so reconciliation is a lookup against the world (§7).
    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError>;
}

/// The value of the `actions/execute` waterfall.
///
/// The outcome rides a shared slot rather than a plain `Option`, because
/// `WaterfallEvent::Value` must be `Clone` and `ActionError` is not — the same arrangement
/// `llm`'s `StreamSlot` uses, for the same reason.
#[derive(Clone)]
pub struct ActionExec {
    pub request: Arc<ExecuteRequest>,
    pub outcome: OutcomeSlot,
}

/// The mutable cell of [`ActionExec`].
#[derive(Clone, Default)]
pub struct OutcomeSlot(Arc<parking_lot::Mutex<Option<Result<ActionArtifact, ActionError>>>>);

impl OutcomeSlot {
    /// An unfilled slot. WP-7.
    pub fn empty() -> OutcomeSlot {
        OutcomeSlot::default()
    }
    /// Fill it, replacing whatever was there. WP-7.
    pub fn put(&self, outcome: Result<ActionArtifact, ActionError>) {
        *self.0.lock() = Some(outcome);
    }
    /// Take the outcome out. WP-7.
    pub fn take(&self) -> Option<Result<ActionArtifact, ActionError>> {
        self.0.lock().take()
    }
    /// Whether some hop filled it.
    pub fn is_filled(&self) -> bool {
        self.0.lock().is_some()
    }
}

/// `actions/execute`.
pub struct ActionsExecute;
impl WaterfallEvent for ActionsExecute {
    const NAME: &'static str = "actions/execute";
    type Value = ActionExec;
}

impl ActionsHandle {
    /// An empty seam: no Provider, so every kind is refused. WP-7.
    pub fn new() -> ActionsHandle {
        ActionsHandle(Arc::new(ActionsInner {
            _providers: parking_lot::Mutex::new(Vec::new()),
        }))
    }

    /// Register a Provider. Registration is an effect (§0.2). WP-7.
    pub async fn provider(
        &self,
        _ctx: &Context,
        _p: Arc<dyn ActionProvider>,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-7: register, with the inverse that removes it")
    }

    /// Intent row + `action/intent` step BEFORE executing; `action/done` + row status after. The
    /// idem key is UNIQUE in the journal, so a concurrent duplicate collides instead of executing
    /// twice (§7).
    ///
    /// WP-7.
    pub async fn execute(
        &self,
        _ctx: &Context,
        _req: ActionRequest,
    ) -> Result<ActionArtifact, ActionError> {
        todo!("WP-7: journal, waterfall, journal again — and NoProvider when nothing is registered")
    }

    /// Boot reconciliation: LISTS intent-without-done rows. Never re-executes (§7, §17 Phase 8).
    ///
    /// WP-7.
    pub async fn pending(&self) -> Result<Vec<PendingAction>, ActionError> {
        todo!("WP-7: list, never act")
    }

    /// Exactly what some Provider registered. Empty in Phase 2. WP-7.
    pub fn kinds(&self) -> Vec<ActionKind> {
        todo!("WP-7")
    }
}

impl Default for ActionsHandle {
    fn default() -> Self {
        ActionsHandle::new()
    }
}

/// No configuration: the four kinds are §7's, not a deployment's.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ActionsConfig {}

/// The Service Definition row.
pub struct ActionsPlugin;

#[async_trait::async_trait]
impl Plugin for ActionsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ActionsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7: provide::<Actions> and list pending rows at boot (list, never re-execute)")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::journal_is_intent_before_done()]
    }
}

bough_kernel::register_plugin!(ActionsPlugin);
