//! Invariant: reconciliation NEVER CALLS A WRITE PATH. For every pending row it performs one
//! read — "is my marker in the world?" — through the same Provider that would have executed it,
//! and then either marks the row done with the located artifact or writes a DRAFT describing the
//! unfinished intent and leaves the row `Intent`. A pending row is never re-executed (§7).
//!
//! P6-D12: [`ArtifactLookup`] is a SECOND trait registered here, because `plugins/actions` is
//! off-limits in this track and `ActionProvider` cannot grow a method. Merge note 2 folds it in.

pub mod invariant;

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bough_kernel::{
    ConfigError, Context, EffectHandle, Inject, InvariantSpec, Plugin, PluginError, ServiceKey,
};
use bough_plugin_actions::{ActionArtifact, ActionError, ActionKind, Actions, ActionsHandle};
use bough_plugin_drafts::{DraftError, DraftKind, DraftsHandle, NewDraft};
use bough_plugin_ledger::{ActionId, ActionStatus, AgentName, WakeId};
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

/// Where an unfinished intent is surfaced. A trait so the reconciler can be tested without the
/// `drafts` row, and so this crate never learns a second way to reach a surface.
#[async_trait::async_trait]
pub trait Drafting: Send + Sync + 'static {
    async fn draft(&self, d: NewDraft) -> Result<(), DraftError>;
}

#[async_trait::async_trait]
impl Drafting for DraftsHandle {
    async fn draft(&self, d: NewDraft) -> Result<(), DraftError> {
        DraftsHandle::draft(self, d).await.map(|_| ())
    }
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
pub struct LookupRegistry(Arc<parking_lot::Mutex<Registered>>);

static NEXT_LOOKUP: AtomicU64 = AtomicU64::new(0);

impl LookupRegistry {
    /// An empty registry.
    pub fn new() -> LookupRegistry {
        LookupRegistry::default()
    }
    /// Register a lookup. An EFFECT: the disposer removes exactly this registration.
    pub async fn register(
        &self,
        ctx: &Context,
        lookup: Arc<dyn ArtifactLookup>,
    ) -> Result<EffectHandle, PluginError> {
        let id = NEXT_LOOKUP.fetch_add(1, Ordering::Relaxed);
        self.0.lock().push((id, lookup));
        let inner = self.0.clone();
        ctx.effect(move |e| async move {
            e.defer_sync(move || {
                inner.lock().retain(|(i, _)| *i != id);
            });
            Ok(())
        })
        .await
    }
    /// The lookup that claims `kind`, if any. Registration order, so a later row does not silently
    /// shadow an earlier one.
    pub fn for_kind(&self, kind: ActionKind) -> Option<Arc<dyn ArtifactLookup>> {
        self.0
            .lock()
            .iter()
            .find(|(_, l)| l.kinds().contains(&kind))
            .map(|(_, l)| l.clone())
    }
    /// How many lookups are registered. The swap test reads it.
    pub fn len(&self) -> usize {
        self.0.lock().len()
    }
    /// Whether nothing is registered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
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
pub struct Reconciler {
    cfg: Arc<ReconcileConfig>,
    actions: ActionsHandle,
    lookups: LookupRegistry,
    drafts: Arc<dyn Drafting>,
}

impl Reconciler {
    /// A reconciler over one seam, one registry and one draft surface.
    pub fn new(
        cfg: Arc<ReconcileConfig>,
        actions: ActionsHandle,
        lookups: LookupRegistry,
        drafts: Arc<dyn Drafting>,
    ) -> Reconciler {
        Reconciler {
            cfg,
            actions,
            lookups,
            drafts,
        }
    }

    /// One pass over `ActionsHandle::pending()`. The clock is INJECTED.
    pub async fn reconcile(&self, now: DateTime<Utc>) -> Result<ReconcileReport, ActionError> {
        let mut report = ReconcileReport::default();
        for p in self.actions.pending().await? {
            let Some(lookup) = self.lookups.for_kind(p.kind) else {
                tracing::warn!(
                    action = %p.action, kind = p.kind.as_str(),
                    "no lookup for this kind: the intent stays open and is not guessed at"
                );
                report.unknown_kind.push(p.action.clone());
                continue;
            };
            match lookup.find_marker(p.kind, &p.target, &p.marker).await? {
                // The act HAPPENED and the crash was between the two writes. Concluding the row is
                // a journal write, never a world write.
                Some(artifact) => {
                    self.actions
                        .ledger()
                        .0
                        .action_done(
                            &p.action,
                            ActionStatus::Done,
                            serde_json::json!({
                                "locator": artifact.locator,
                                "marker": artifact.marker,
                                "detail": artifact.detail,
                                "reconciled": true,
                            }),
                            now,
                        )
                        .await?;
                    report.marked_done.push(p.action);
                }
                // The marker is NOT in the world. It is not re-executed: a person decides.
                None => {
                    let body = format!(
                        "An action was journalled and never concluded, and its marker is not in \
                         the world:\n\n- kind: {}\n- target: {}\n- marker: {}\n- intended at: {}\n\n\
                         Nothing was re-executed. Decide whether to do it by hand.",
                        p.kind.as_str(),
                        p.target,
                        p.marker,
                        p.at
                    );
                    self.drafts
                        .draft(NewDraft {
                            kind: DraftKind::Message,
                            agent: AgentName::new(&self.cfg.surface_to),
                            wake: WakeId::new(format!("reconcile:{}", p.action)),
                            audience: "human:andrey".into(),
                            subject: format!("unfinished {} on {}", p.kind.as_str(), p.target),
                            body,
                            refs: Default::default(),
                            at: now,
                        })
                        .await
                        .map_err(|e| ActionError::Provider {
                            kind: p.kind.as_str(),
                            source: anyhow::Error::new(e),
                        })?;
                    report.surfaced.push(p.action);
                }
            }
        }
        Ok(report)
    }
}

/// The row.
pub struct ReconcilePlugin;

#[async_trait::async_trait]
impl Plugin for ReconcilePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ReconcileConfig;

    fn inject() -> Inject {
        // `ledger`: the runtime invariant folds this row's own action rows, so the read has to be
        // declared (the actions seam already requires the ledger, so this never widens a tree).
        Inject::required(["actions", "drafts", "agents", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.surface_to.trim().is_empty() {
            return Err(ConfigError::Rejected {
                detail: "surface_to: name the agent whose lane an unfinished intent is drafted on"
                    .into(),
            });
        }
        Ok(())
    }

    /// Provide the lookup registry FIRST (the two action Providers inject it), then run one pass
    /// when `at_boot`.
    ///
    /// DEVIATION: the boot pass runs on a DEFERRED effect, after the tree has loaded, because the
    /// Providers that register the lookups mount after this row provides the key.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let registry = LookupRegistry::new();
        ctx.provide::<ActionLookup>(registry.clone())
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        let actions = ctx
            .get::<Actions>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let drafts = ctx
            .get::<bough_plugin_drafts::Drafts>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // The referent must exist, and a missing one is LOUD rather than a silent skip (§0.2).
        let agents = ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        if agents.by_name(&AgentName::new(&cfg.surface_to)).is_none() {
            tracing::warn!(
                agent = %cfg.surface_to,
                "actions-reconcile: `surface_to` names no live agent; unfinished intents would \
                 have nowhere to surface"
            );
        }

        if cfg.at_boot {
            let reconciler = Reconciler::new(
                cfg.clone(),
                (*actions).clone(),
                registry,
                Arc::new((*drafts).clone()) as Arc<dyn Drafting>,
            );
            // An ORDINARY EFFECT, so the pass belongs to this row's life and disposing the row
            // takes it with it. It runs after `apply` returns, which is what lets the Providers
            // that register the lookups mount first.
            ctx.effect_spawn(move |ectx| async move {
                if ectx.checkpoint().await.is_err() {
                    return Ok(());
                }
                match reconciler.reconcile(Utc::now()).await {
                    Ok(r) if r == ReconcileReport::default() => {}
                    Ok(r) => tracing::info!(
                        done = r.marked_done.len(),
                        surfaced = r.surfaced.len(),
                        unknown = r.unknown_kind.len(),
                        "reconciled interrupted actions"
                    ),
                    Err(e) => tracing::warn!(error = %e, "reconciliation pass failed"),
                }
                Ok(())
            });
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ReconcilePlugin);
