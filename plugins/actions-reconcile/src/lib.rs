//! Invariant: reconciliation NEVER CALLS A WRITE PATH. For every pending row it performs one
//! read — "is my marker in the world?" — through the same Provider that would have executed it,
//! and then either marks the row done with the located artifact or writes a DRAFT describing the
//! unfinished intent and leaves the row `Intent`. A pending row is never re-executed (§7).
//!
//! MERGE (note 2, P6-D12 CLOSED): the lookup used to be a SECOND trait (`ArtifactLookup`) with a
//! registry of its own under an `action_lookup` key, because `plugins/actions` was off-limits to
//! track B. `ActionProvider::find_marker` is now the one place a kind's lookup lives, so this row
//! is a pure CONSUMER of the `actions` seam: it lists, it looks up through the seam, it concludes
//! or it drafts. Merge note §18 likewise collapses the `Drafting` trait onto `DraftsHandle` — the
//! tests read the draft back out of the ledger, which is the durable fact anyway.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_actions::{ActionError, Actions, ActionsHandle};
use bough_plugin_drafts::{DraftKind, DraftsHandle, NewDraft};
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

/// What one reconciliation pass did.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReconcileReport {
    pub marked_done: Vec<ActionId>,
    pub surfaced: Vec<ActionId>,
    /// Pending rows whose kind no Provider claims. Reported, never guessed at.
    pub unknown_kind: Vec<ActionId>,
}

/// The reconciler: one seam, one draft surface.
pub struct Reconciler {
    cfg: Arc<ReconcileConfig>,
    actions: ActionsHandle,
    drafts: DraftsHandle,
}

impl Reconciler {
    /// A reconciler over one actions seam and one draft surface.
    pub fn new(
        cfg: Arc<ReconcileConfig>,
        actions: ActionsHandle,
        drafts: DraftsHandle,
    ) -> Reconciler {
        Reconciler {
            cfg,
            actions,
            drafts,
        }
    }

    /// One pass over `ActionsHandle::pending()`. The clock is INJECTED.
    pub async fn reconcile(&self, now: DateTime<Utc>) -> Result<ReconcileReport, ActionError> {
        let mut report = ReconcileReport::default();
        for p in self.actions.pending().await? {
            // A kind with no live Provider is REPORTED and left alone: the row is not concluded
            // and not drafted as if it had been examined, because nothing examined it.
            let found = match self.actions.find_marker(p.kind, &p.target, &p.marker).await {
                Ok(found) => found,
                Err(ActionError::NoProvider(_)) => {
                    tracing::warn!(
                        action = %p.action, kind = p.kind.as_str(),
                        "no provider for this kind: the intent stays open and is not guessed at"
                    );
                    report.unknown_kind.push(p.action.clone());
                    continue;
                }
                Err(e) => return Err(e),
            };
            match found {
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

    /// Run one pass when `at_boot`.
    ///
    /// DEVIATION: the boot pass runs on a DEFERRED effect, after the tree has loaded, because the
    /// Providers this row looks up through mount after it.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let actions = ctx
            .get::<Actions>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let drafts = ctx
            .get::<bough_plugin_drafts::Drafts>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let agents = ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        if cfg.at_boot {
            let reconciler = Reconciler::new(cfg.clone(), (*actions).clone(), (*drafts).clone());
            let surface_to = AgentName::new(&cfg.surface_to);
            let agents = (*agents).clone();
            // An ORDINARY EFFECT, so the pass belongs to this row's life and disposing the row
            // takes it with it. It runs after `apply` returns, which is what lets the Providers
            // it looks up through mount first.
            ctx.effect_spawn(move |ectx| async move {
                if ectx.checkpoint().await.is_err() {
                    return Ok(());
                }
                match reconciler.reconcile(Utc::now()).await {
                    Ok(r) if r == ReconcileReport::default() => {}
                    Ok(r) => {
                        // MERGE (track B -> Phase 5): the referent check lives HERE, not at apply
                        // time. `residents` raises the roster asynchronously AFTER the rows load,
                        // so an apply-time `by_name` said "`surface_to` names no live agent" on
                        // every single boot of the shipped TUI tree about an agent that appeared a
                        // moment later — a false alarm on a line whose whole value is being
                        // believed. It is still LOUD and never a silent skip (§0.2); it is loud at
                        // the moment it costs something, which is a pass that found unfinished
                        // intents with nowhere to put them.
                        if agents.by_name(&surface_to).is_none() {
                            tracing::warn!(
                                agent = %surface_to,
                                "actions-reconcile: `surface_to` names no live agent; \
                                 unfinished intents have nowhere to surface"
                            );
                        }
                        tracing::info!(
                            done = r.marked_done.len(),
                            surfaced = r.surfaced.len(),
                            unknown = r.unknown_kind.len(),
                            "reconciled interrupted actions"
                        );
                    }
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
