//! Invariant: this is a ledger PROVIDER (§0.2). It owns storage and nothing else: the vocabulary,
//! the rules and the conformance suite belong to `bough-plugin-ledger`, which this crate depends
//! on and which never depends back. Its bundle row is `ledger-sqlite`.
//!

pub mod append;
pub mod connected;
pub mod fork;
pub mod invariant;
pub mod read;
pub mod schema;
pub mod search;
pub mod store;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::*;

use crate::store::SqliteStore;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "ledger-sqlite";

/// The row's config (§0.5: validated purely, no I/O in `validate`).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqliteConfig {
    /// The db file. `":memory:"` is allowed and skips WAL.
    pub path: PathBuf,
    /// How long a writer waits on a locked db before giving up.
    #[serde(default = "default_busy_timeout")]
    pub busy_timeout_ms: u64,
}

fn default_busy_timeout() -> u64 {
    5000
}

/// The provider plugin.
pub struct SqliteLedgerPlugin;

#[async_trait::async_trait]
impl Plugin for SqliteLedgerPlugin {
    const NAME: &'static str = "ledger-sqlite";
    type Config = SqliteConfig;

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        // Purely, synchronously, no I/O (§0.5): the file is opened in `apply`, not here.
        if cfg.path.as_os_str().is_empty() {
            return Err(ConfigError::Rejected {
                detail: "path: the ledger needs a db file path, or `:memory:`".into(),
            });
        }
        if cfg.busy_timeout_ms == 0 {
            return Err(ConfigError::Rejected {
                detail: "busy_timeout_ms: 0 would make every contended write fail immediately"
                    .into(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        // Opening fails LOUD: a ledger whose format this binary does not speak is a boot failure,
        // never a silent migration (§0.5).
        let store = SqliteStore::open(&cfg, ctx.clone())
            .map_err(|e| PluginError::new(entry.clone(), anyhow::Error::new(e)))?;

        // Teardown, in order (phase ux1 §2.10, M28): CHECKPOINT the WAL back into the db, THEN
        // poison the store. A relaunch after an unclosed shutdown was reading a 4.1k db beside a
        // 231k WAL, which is how the audit lost a session's history.
        let retire_me = store.clone();
        ctx.effect(move |e| async move {
            e.defer(move || {
                let store = retire_me.clone();
                async move {
                    let _ = store.checkpoint().await;
                    store.retire();
                }
            });
            Ok(())
        })
        .await?;

        ctx.provide::<Ledger>(LedgerHandle(store))
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // The stream `invariant.rs` polices. Recorded per LIFE: a reload keeps the FiberUid, so
        // this fiber's observations are forgotten when it unloads (the `hello` lesson, §0.3).
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || bough_plugin_ledger::invariant::forget(mine));
            Ok(())
        })
        .await?;
        ctx.on::<LedgerStep, _, _>(move |step| async move {
            bough_plugin_ledger::invariant::record(bough_plugin_ledger::invariant::Obs {
                fiber: mine,
                traj: step.traj.clone(),
                seq: step.seq,
                wake: step.wake.clone(),
                kind: step.kind.clone(),
            });
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SqliteLedgerPlugin);

#[async_trait::async_trait]
impl LedgerStore for SqliteStore {
    fn provider(&self) -> &'static str {
        SqliteLedgerPlugin::NAME
    }
    fn format_version(&self) -> u32 {
        LEDGER_FORMAT_VERSION
    }

    fn register_step_type(&self, def: StepTypeDef) -> Result<StepTypeToken, LedgerError> {
        self.types.register(def)
    }
    fn step_types(&self) -> Vec<StepTypeDef> {
        self.types.all()
    }
    fn skipped_ignorable(&self) -> u64 {
        self.skipped.load(std::sync::atomic::Ordering::Relaxed)
    }

    async fn append(&self, req: Append) -> Result<Step, LedgerError> {
        crate::append::append(self, req).await
    }
    async fn append_batch(&self, reqs: Vec<Append>) -> Result<Vec<Step>, LedgerError> {
        crate::append::append_batch(self, reqs).await
    }

    async fn step(&self, id: &StepId) -> Result<Option<Step>, LedgerError> {
        crate::read::step(self, id).await
    }
    async fn steps(&self, q: &StepQuery) -> Result<Vec<Step>, LedgerError> {
        crate::read::steps(self, q).await
    }
    async fn tail(&self, traj: &TrajId, n: usize) -> Result<Vec<Step>, LedgerError> {
        crate::read::tail(self, traj, n).await
    }
    async fn head_seq(&self, traj: &TrajId) -> Result<Option<Seq>, LedgerError> {
        crate::read::head_seq(self, traj).await
    }
    async fn search(&self, q: &SearchQuery) -> Result<Vec<SearchHit>, LedgerError> {
        crate::search::search(self, q).await
    }
    async fn live_pins(&self, trajs: &[TrajId]) -> Result<Vec<Pin>, LedgerError> {
        crate::read::live_pins(self, trajs).await
    }
    async fn unconsumed_mail(&self, traj: &TrajId) -> Result<Vec<Step>, LedgerError> {
        crate::read::unconsumed_mail(self, traj).await
    }

    async fn add_edge(&self, e: Edge) -> Result<(), LedgerError> {
        crate::read::add_edge(self, e).await
    }
    async fn edges(&self, traj: &TrajId) -> Result<Vec<Edge>, LedgerError> {
        crate::read::edges(self, traj).await
    }
    async fn ancestry(&self, traj: &TrajId) -> Result<Vec<TrajId>, LedgerError> {
        crate::read::ancestry(self, traj).await
    }
    async fn fork(&self, req: Fork) -> Result<ForkOutcome, LedgerError> {
        crate::fork::fork(self, req).await
    }
    async fn connected(&self, agent: &AgentName) -> Result<Connected, LedgerError> {
        crate::connected::connected(self, agent).await
    }

    async fn seal_rollup(&self, r: NewRollup) -> Result<Rollup, LedgerError> {
        crate::read::seal_rollup(self, r).await
    }
    async fn supersede_rollup(&self, old: &RollupId, new: &RollupId) -> Result<(), LedgerError> {
        crate::read::supersede_rollup(self, old, new).await
    }
    async fn rollups(&self, q: &RollupQuery) -> Result<Vec<Rollup>, LedgerError> {
        crate::read::rollups(self, q).await
    }

    async fn put_agent(&self, a: AgentRow) -> Result<(), LedgerError> {
        crate::read::put_agent(self, a).await
    }
    async fn agent(&self, name: &AgentName) -> Result<Option<AgentRow>, LedgerError> {
        crate::read::agent(self, name).await
    }
    async fn agents(&self) -> Result<Vec<AgentRow>, LedgerError> {
        crate::read::agents(self).await
    }
    async fn delete_agent(&self, name: &AgentName) -> Result<(), LedgerError> {
        crate::read::delete_agent(self, name).await
    }

    async fn action_intent(&self, a: NewAction) -> Result<ActionRow, LedgerError> {
        let (traj, target) = (a.traj.clone(), a.target.clone());
        let row = crate::read::action_intent(self, a).await?;
        // §2.7 item 4: the journal row and the ledger step are one act. The step goes second so a
        // crash between them leaves an intent row with no step, which reconciliation reports —
        // never a step claiming an action the journal does not know.
        self.append(bough_plugin_ledger::journal::intent_step(
            &row, &traj, &target,
        ))
        .await?;
        Ok(row)
    }
    async fn action_done(
        &self,
        id: &ActionId,
        status: ActionStatus,
        result: serde_json::Value,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), LedgerError> {
        crate::read::action_done(self, id, status, result.clone(), at).await?;
        crate::actions_done_step(self, id, status, &result, at).await
    }
    async fn actions(&self, q: &ActionQuery) -> Result<Vec<ActionRow>, LedgerError> {
        crate::read::actions(self, q).await
    }

    async fn row_hashes(&self, scope: HashScope) -> Result<Vec<RowHash>, LedgerError> {
        crate::read::row_hashes(self, scope).await
    }
    async fn trajectory_view(&self, traj: &TrajId) -> Result<TrajectoryView, LedgerError> {
        crate::read::trajectory_view(self, traj).await
    }
}

/// The `action/done` step both providers append, once the journal row is updated.
///
/// Lives here rather than in each `impl` so the two stores cannot disagree about which
/// trajectory the step lands in or what it cites.
pub(crate) async fn actions_done_step(
    store: &dyn LedgerStore,
    id: &ActionId,
    status: ActionStatus,
    result: &serde_json::Value,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), LedgerError> {
    let row = store
        .actions(&bough_plugin_ledger::ActionQuery {
            ids: vec![id.clone()],
            ..Default::default()
        })
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| LedgerError::Store(anyhow::anyhow!("no such action `{id}`")))?;
    let Some(intent) = bough_plugin_ledger::journal::find_intent_step(store, id).await? else {
        return Err(LedgerError::Store(anyhow::anyhow!(
            "action `{id}` has no `action/intent` step to close"
        )));
    };
    store
        .append(bough_plugin_ledger::journal::done_step(
            &row,
            &intent.traj,
            &intent.id,
            status,
            bough_plugin_ledger::journal::artifact_of(result),
            at,
        ))
        .await?;
    Ok(())
}
