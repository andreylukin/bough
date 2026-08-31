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

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bough_kernel::{
    Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey, WaterfallEvent,
};
use bough_plugin_ledger::{
    ActionQuery, ActionRow, ActionStatus, AgentName, IdemKey, Ledger, LedgerHandle, NewAction,
    TrajId,
};

pub use error::ActionError;
pub use journal::{
    idem_key, marker_for, ActionArtifact, ActionRequest, ActionRequestParts, ExecuteRequest,
    PendingAction,
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

/// The seam's live state: which kinds have a Provider, and the journal they write into.
pub struct ActionsInner {
    /// Registration order, each paired with the id its disposer removes. Empty in Phase 2, on
    /// purpose (§17 Phase 6).
    providers: parking_lot::Mutex<Vec<(u64, Arc<dyn ActionProvider>)>>,
    ledger: LedgerHandle,
}

/// What an action Provider does. Phase 6 writes them.
#[async_trait::async_trait]
pub trait ActionProvider: Send + Sync + 'static {
    fn kinds(&self) -> Vec<ActionKind>;
    /// The Provider embeds `req.marker` in the artifact itself (PR body, commit trailer, comment
    /// suffix) so reconciliation is a lookup against the world (§7).
    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError>;

    /// The other half of §7's reconciliation: is this action's marker IN THE WORLD? A READ,
    /// always — a Provider that acts here is a Provider that re-executed a pending intent.
    ///
    /// Merge note 2: this used to be a SECOND trait (`actions-reconcile::ArtifactLookup`) with a
    /// registry of its own, because track B could not edit this crate. One Provider, one
    /// registry, one place a kind lives.
    ///
    /// The default answers `Ok(None)`: a Provider that cannot look its artifact up says so by
    /// finding nothing, and the intent is then SURFACED rather than concluded — the safe
    /// direction, because concluding a row is what stops a person from ever seeing it.
    async fn find_marker(
        &self,
        _kind: ActionKind,
        _canonical_target: &str,
        _marker: &str,
    ) -> Result<Option<ActionArtifact>, ActionError> {
        Ok(None)
    }
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
    /// An empty seam over one ledger: no Provider, so every kind is refused.
    pub fn new(ledger: LedgerHandle) -> ActionsHandle {
        ActionsHandle(Arc::new(ActionsInner {
            providers: parking_lot::Mutex::new(Vec::new()),
            ledger,
        }))
    }

    /// The ledger this seam journals into.
    pub fn ledger(&self) -> &LedgerHandle {
        &self.0.ledger
    }

    /// Register a Provider. Registration is an effect (§0.2): the inverse removes exactly this
    /// registration, so unloading a Provider row makes its kinds stop existing again.
    pub async fn provider(
        &self,
        ctx: &Context,
        p: Arc<dyn ActionProvider>,
    ) -> Result<EffectHandle, PluginError> {
        let id = NEXT_PROVIDER.fetch_add(1, Ordering::Relaxed);
        self.0.providers.lock().push((id, p));
        let inner = self.0.clone();
        // The set of performable kinds just CHANGED, and a Consumer that offers one tool per kind
        // has to hear about it. Registration mutates this handle IN PLACE, so the `actions`
        // service binding does not change and §0.3's activation-driven reload never fires — this
        // event is the seam that stands in for it (§0.2: a capability event attaches policy to a
        // seam without importing the loop).
        ctx.emit::<ProvidersChangedEvent>(ProvidersChanged {
            kinds: ActionsHandle(self.0.clone()).kinds(),
        });
        let ectx = ctx.clone();
        ctx.effect(move |e| async move {
            e.defer_sync(move || {
                inner.providers.lock().retain(|(i, _)| *i != id);
                let kinds = ActionsHandle(inner.clone()).kinds();
                ectx.emit::<ProvidersChangedEvent>(ProvidersChanged { kinds });
            });
            Ok(())
        })
        .await
    }

    /// Intent row + `action/intent` step BEFORE executing; `action/done` + row status after. The
    /// idem key is UNIQUE in the journal, so a concurrent duplicate collides instead of executing
    /// twice (§7).
    pub async fn execute(
        &self,
        ctx: &Context,
        req: ActionRequest,
    ) -> Result<ActionArtifact, ActionError> {
        let kind = req.kind;
        let canonical = req.target.canonical(kind)?;

        // A kind no Provider registered DOES NOT EXIST (§7). Refused here, before anything is
        // journalled: an intent row is a promise that something was attempted on the world, and
        // nothing was.
        let provider = self
            .provider_for(kind)
            .ok_or(ActionError::NoProvider(kind.as_str()))?;

        let idem = idem_key(kind, &canonical, &req.step);
        if let Some(row) = self.row_with_idem_key(&idem).await? {
            return Err(ActionError::Duplicate {
                kind: kind.as_str(),
                target: canonical,
                step: req.step.clone(),
                action: row.id,
            });
        }

        let traj = self.traj_of(&req.agent).await?;
        let marker = marker_for(&idem);
        let payload = serde_json::json!({
            "target": canonical,
            "raw_target": req.target.raw,
            "payload": req.payload,
            "marker": marker,
        });

        // ---- BEFORE: the journal row, which is also the `action/intent` step -------------
        // One act, in the store (§2.7 item 4): there is no window in which a row exists without
        // its step, so "intent before done" is a property of the ledger, not of a caller's care.
        let row = self
            .0
            .ledger
            .0
            .action_intent(NewAction {
                id: None,
                traj,
                wake: req.wake.clone(),
                target: canonical.clone(),
                idem_key: idem.clone(),
                kind: kind.as_str().to_string(),
                payload: payload.clone(),
                at: req.at,
            })
            .await?;

        // ---- the waterfall, then the Provider ---------------------------------------------
        let exec = Arc::new(ExecuteRequest {
            request: Arc::new(req),
            action: row.id.clone(),
            idem_key: idem,
            marker,
            canonical_target: canonical,
        });
        let hopped = ctx
            .waterfall::<ActionsExecute>(ActionExec {
                request: exec.clone(),
                outcome: OutcomeSlot::empty(),
            })
            .await;
        let outcome = match hopped.outcome.take() {
            Some(o) => o,
            None => provider.execute(&exec).await,
        };

        // ---- AFTER: the row's status, which is also the `action/done` step ----------------
        let (status, result) = match &outcome {
            Ok(a) => (
                ActionStatus::Done,
                serde_json::json!({ "locator": a.locator, "marker": a.marker, "detail": a.detail }),
            ),
            // A Provider that FAILED still concluded: the row is marked `failed` and the
            // `action/done` step is written, so this action never shows up as unreconciled work.
            Err(e) => (
                ActionStatus::Failed,
                serde_json::json!({ "error": e.to_string() }),
            ),
        };
        self.0
            .ledger
            .0
            .action_done(&exec.action, status, result, exec.request.at)
            .await?;
        outcome
    }

    /// [`ActionsHandle::execute`] for a kind that is still a STRING (merge note 3, V10).
    ///
    /// A name outside §7's four is refused HERE, by the executor, as
    /// [`ActionError::NoSuchKind`] — so "there is no `slack_send`" is the actions seam answering
    /// about its own vocabulary rather than a caller's parser refusing one step earlier. Nothing
    /// is journalled for a name that does not exist: an intent row is a promise that something
    /// was attempted on the world.
    pub async fn execute_by_name(
        &self,
        ctx: &Context,
        kind: &str,
        req: ActionRequestParts,
    ) -> Result<ActionArtifact, ActionError> {
        let resolved = ActionKind::parse(kind).ok_or_else(|| ActionError::NoSuchKind {
            name: kind.to_string(),
            known: ActionKind::KNOWN,
        })?;
        self.execute(ctx, req.with_kind(resolved)).await
    }

    /// Is this action's marker in the world? Routed to the Provider that owns the kind
    /// (merge note 2). A READ: nothing here writes to the world or to the journal.
    pub async fn find_marker(
        &self,
        kind: ActionKind,
        canonical_target: &str,
        marker: &str,
    ) -> Result<Option<ActionArtifact>, ActionError> {
        let provider = self
            .provider_for(kind)
            .ok_or(ActionError::NoProvider(kind.as_str()))?;
        provider.find_marker(kind, canonical_target, marker).await
    }

    /// Boot reconciliation: LISTS intent-without-done rows. Never re-executes (§7, §17 Phase 8).
    pub async fn pending(&self) -> Result<Vec<PendingAction>, ActionError> {
        let rows = self
            .0
            .ledger
            .0
            .actions(&ActionQuery {
                status: Some(ActionStatus::Intent),
                ..Default::default()
            })
            .await?;
        Ok(rows.iter().filter_map(pending_of).collect())
    }

    /// Exactly what some Provider registered. Empty in Phase 2, on purpose.
    pub fn kinds(&self) -> Vec<ActionKind> {
        let mut out: Vec<ActionKind> = Vec::new();
        for (_, p) in self.0.providers.lock().iter() {
            for k in p.kinds() {
                if !out.contains(&k) {
                    out.push(k);
                }
            }
        }
        out
    }

    /// The first Provider that claims `kind`. Registration order, so a later row does not
    /// silently shadow an earlier one.
    fn provider_for(&self, kind: ActionKind) -> Option<Arc<dyn ActionProvider>> {
        self.0
            .providers
            .lock()
            .iter()
            .find(|(_, p)| p.kinds().contains(&kind))
            .map(|(_, p)| p.clone())
    }

    async fn row_with_idem_key(&self, idem: &IdemKey) -> Result<Option<ActionRow>, ActionError> {
        // Merge note 5: `ActionQuery` now carries the idem key, so this is a one-row lookup and
        // not a scan of the journal — which matters because reconciliation asks it once per
        // pending row.
        let rows = self
            .0
            .ledger
            .0
            .actions(&ActionQuery {
                idem_key: Some(idem.clone()),
                limit: Some(1),
                ..Default::default()
            })
            .await?;
        Ok(rows.into_iter().next())
    }

    async fn traj_of(&self, agent: &AgentName) -> Result<TrajId, ActionError> {
        match self.0.ledger.0.agent(agent).await? {
            Some(row) => Ok(row.traj),
            None => Err(ActionError::UnknownAgent(agent.clone())),
        }
    }
}

/// The journal row of an action that was attempted and never concluded, as reconciliation reads
/// it. `None` for a row this seam did not write (no marker in its payload).
fn pending_of(row: &ActionRow) -> Option<PendingAction> {
    Some(PendingAction {
        action: row.id.clone(),
        kind: ActionKind::all()
            .iter()
            .copied()
            .find(|k| k.as_str() == row.kind)?,
        idem_key: row.idem_key.clone(),
        target: row.payload.get("target")?.as_str()?.to_string(),
        marker: marker_for(&row.idem_key),
        at: row.at,
    })
}

static NEXT_PROVIDER: AtomicU64 = AtomicU64::new(0);

/// `actions/providers-changed` — EMIT (a CAPABILITY event, §0.2). Raised whenever a Provider is
/// registered or its registration is disposed. Providers register INTO this handle rather than by
/// re-providing the `actions` key, so a Consumer cannot learn about them from the fiber lifecycle;
/// this is how it learns instead.
pub struct ProvidersChangedEvent;

impl bough_kernel::EmitEvent for ProvidersChangedEvent {
    const NAME: &'static str = "actions/providers-changed";
    type Payload = ProvidersChanged;
}

/// The kinds that have a live Provider, after the change.
#[derive(Clone, Debug, PartialEq)]
pub struct ProvidersChanged {
    pub kinds: Vec<ActionKind>,
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

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let handle = ActionsHandle::new((*ledger).clone());

        // Reconciliation at boot LISTS and never acts (§7): an intent without a done is a lookup
        // against the world, and the world is looked at by a person or by Phase 8, never here.
        match handle.pending().await {
            Ok(rows) if rows.is_empty() => {}
            Ok(rows) => {
                for r in &rows {
                    tracing::warn!(
                        action = %r.action, kind = r.kind.as_str(), target = %r.target,
                        marker = %r.marker,
                        "action intent with no done: reconcile by looking for the marker"
                    );
                }
            }
            Err(e) => tracing::warn!(error = %e, "could not list pending actions at boot"),
        }

        ctx.provide::<Actions>(handle)
            .await
            .map_err(|e| PluginError::new(entry, e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::journal_is_intent_before_done()]
    }
}

bough_kernel::register_plugin!(ActionsPlugin);
