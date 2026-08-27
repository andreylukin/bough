//! Invariant: at most ONE catch-up wake per agent per activation, and none at all for an agent
//! with nothing queued (§5, V6). TUI launch is the lid-open proxy (§13: there is no lid
//! notification on macOS; Phase 7's `sleep-listener` row will call the same method).
//!
//! The row holds every resumed agent's `AgentDisposer` inside its own effect, so disabling
//! `residents` by patch tears the roster down and leaves the ledger untouched (P3-D17).

pub mod invariant;

pub use bough_util::text::one_sentence;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{
    AgentDisposer, AgentKind, Agents, AgentsHandle, CreateAgent, ResumeAgent, WakeCause, WakeKind,
    WakeRequest,
};
use bough_plugin_ledger::{AgentName, Ledger, LedgerHandle, TrajId};
use parking_lot::Mutex;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "residents";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ResidentsConfig {
    /// Agent names to CREATE when the ledger has no row for them. Empty ⇒ create nothing.
    pub bootstrap: Vec<String>,
    /// Trajectory id prefix for a bootstrapped agent: `lane/` + name.
    pub traj_prefix: String,
    /// Resume every `agents` row at launch and hold its disposer.
    pub resume_all: bool,
    /// Run §5's catch-up wake once the roster is up.
    pub catch_up: bool,
}

/// PURE: which agents get a catch-up wake, given the roster and each one's unconsumed mail.
/// Empty for an agent with nothing queued — that is V6's "and none when nothing is queued".
pub fn catch_up_set(roster: &[(AgentName, usize)]) -> Vec<AgentName> {
    roster
        .iter()
        .filter(|(_, queued)| *queued > 0)
        .map(|(name, _)| name.clone())
        .collect()
}

/// How long the row waits for a loop Provider to take the factory slot. A protocol bound on a
/// startup race, not a deployment value (§0.2) — the `exec` row's `wait_for_factory` precedent:
/// row order carries no load semantics, so waiting is the row's job, and waiting FOREVER would
/// turn a missing loop row into a hang instead of the boot failure it is.
const FACTORY_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

pub async fn wait_for_factory(agents: &AgentsHandle) -> Result<(), String> {
    wait_for_factory_until(agents, std::time::Instant::now() + FACTORY_WAIT).await
}

/// `create` / `resume` against a slot that can EMPTY AGAIN after [`wait_for_factory`] returned:
/// the loop row reloads when one of its injected keys changes provider (§0.3) — a `--patch` that
/// swaps `llm.anthropic` for `llm-replay`, or adds a row, does that during boot — and its disposer
/// nulls the factory until the reload re-sets it. So `NoFactory` from the call itself is the same
/// startup race as an unfilled slot and gets the same deadline; any other error is final. Seen
/// live as a blank TUI: the whole boot task died on the first `create`, so no roster came up
/// (`scripts/tui/07-old-feed.sh` under the unoptimized binary).
async fn with_factory<T, F, Fut>(
    agents: &AgentsHandle,
    deadline: std::time::Instant,
    mut call: F,
) -> Result<T, String>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, bough_plugin_agents::AgentError>>,
{
    loop {
        match call().await {
            Err(bough_plugin_agents::AgentError::NoFactory)
                if std::time::Instant::now() < deadline =>
            {
                wait_for_factory_until(agents, deadline).await?;
            }
            other => return other.map_err(|e| e.to_string()),
        }
    }
}

async fn wait_for_factory_until(
    agents: &AgentsHandle,
    deadline: std::time::Instant,
) -> Result<(), String> {
    while agents.factory().is_none() {
        if std::time::Instant::now() >= deadline {
            return Err(format!(
                "no agent factory after {FACTORY_WAIT:?}; mount an `agent-loop` row"
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    Ok(())
}

/// The roster this row holds. Disposing it tears every resumed agent down and leaves the ledger
/// alone: an agent's trajectory outlives its handle (`AgentDisposer::dispose` step 4).
#[derive(Default)]
pub struct Roster(Mutex<Vec<AgentDisposer>>);

impl Roster {
    /// How many agents the row is holding.
    pub fn len(&self) -> usize {
        self.0.lock().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn push(&self, d: AgentDisposer) {
        self.0.lock().push(d);
    }
    /// Tear ONE agent down and stop holding it. Returns whether it was held.
    pub async fn dispose_named(&self, name: &AgentName) -> bool {
        let held = {
            let mut roster = self.0.lock();
            roster
                .iter()
                .position(|d| d.agent().name() == name)
                .map(|i| roster.remove(i))
        };
        match held {
            Some(d) => {
                d.dispose().await;
                true
            }
            None => false,
        }
    }
    /// Tear the whole roster down. The ledger is untouched: an agent's trajectory outlives its
    /// handle.
    pub async fn dispose_all(&self) {
        let held: Vec<AgentDisposer> = std::mem::take(&mut *self.0.lock());
        for d in held {
            d.dispose().await;
        }
    }
}

/// Bring the roster up: bootstrap what the ledger does not have, resume what it does.
///
/// Bootstrapping is per NAME: `create` is only reached for a configured name with no `agents`
/// row, so a second launch resumes the lane it made the first time instead of minting a new one.
pub async fn raise_roster(
    agents: &AgentsHandle,
    ledger: &LedgerHandle,
    cfg: &ResidentsConfig,
    roster: &Roster,
) -> Result<Vec<AgentName>, String> {
    let now = chrono::Utc::now();
    let deadline = std::time::Instant::now() + FACTORY_WAIT;
    let mut up: Vec<AgentName> = Vec::new();

    for name in &cfg.bootstrap {
        let name = AgentName::new(name);
        let existing = ledger.0.agent(&name).await.map_err(|e| e.to_string())?;
        if existing.is_some() {
            continue;
        }
        let traj = TrajId::new(format!("{}{}", cfg.traj_prefix, name));
        let (agent, disposer) = with_factory(agents, deadline, || {
            agents.create(CreateAgent {
                name: name.clone(),
                traj: traj.clone(),
                kind: AgentKind::Resident,
                scope: None,
                setup: None,
                seed: Vec::new(),
                at: now,
            })
        })
        .await?;
        roster.push(disposer);
        up.push(agent.name().clone());
    }

    if cfg.resume_all {
        for row in ledger.0.agents().await.map_err(|e| e.to_string())? {
            if agents.by_name(&row.name).is_some() {
                continue;
            }
            let (agent, disposer) = with_factory(agents, deadline, || {
                agents.resume(ResumeAgent {
                    name: row.name.clone(),
                    at: now,
                    setup: None,
                })
            })
            .await?;
            roster.push(disposer);
            up.push(agent.name().clone());
        }
    }
    Ok(up)
}

/// Bring the LIVE registry back into line with the `agents` ROWS after a structural op.
///
/// §3 makes the rows mutable config and `Agent::traj()` immutable for an agent's life, so the two
/// halves genuinely come apart: after a merge the survivor's row points at the new head while its
/// live agent still appends to the pre-merge chain, and the absorbed agent runs with no row at
/// all; after a split or a bud the children have rows and no agent, so mail matched to their
/// `routing_refs` finds `by_name(..) == None` and is not delivered. Nothing else reconciles them —
/// `graph-ops` does not own the disposers — so this row, which does, listens for the fact.
///
/// Total and idempotent: a row that is already live on its own trajectory is left alone.
pub async fn reconcile_rows(
    agents: &AgentsHandle,
    ledger: &LedgerHandle,
    roster: &Roster,
    changed: &bough_plugin_agents::RowsChanged,
) -> Result<Vec<AgentName>, String> {
    let now = chrono::Utc::now();
    let mut touched = Vec::new();

    // A deleted row means the agent is gone: an absorbed lane must stop running.
    for name in &changed.deleted {
        if roster.dispose_named(name).await {
            touched.push(name.clone());
        }
    }

    for name in &changed.written {
        let Some(row) = ledger.0.agent(name).await.map_err(|e| e.to_string())? else {
            continue;
        };
        match agents.by_name(name) {
            // Already live on the trajectory its row names: nothing to do.
            Some(live) if *live.traj() == row.traj => continue,
            // Live on a DIFFERENT trajectory — a merge moved the row's head. The agent has to be
            // torn down and resumed, because a live agent's trajectory never changes.
            //
            // A `false` here means somebody ELSE holds this agent's disposer: tearing it down is
            // not ours to do, and resuming a second one under the same name would be refused
            // anyway.
            Some(_) if !roster.dispose_named(name).await => {
                tracing::warn!(
                    agent = %name,
                    "residents: row moved trajectory but its disposer is held elsewhere"
                );
                continue;
            }
            Some(_) => {}
            None => {}
        }
        let (_, disposer) = agents
            .resume(ResumeAgent {
                name: name.clone(),
                at: now,
                setup: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        roster.push(disposer);
        touched.push(name.clone());
    }
    Ok(touched)
}

/// §5's catch-up: ONE wake per agent that has queued mail, and nothing at all for one that does
/// not. The set is computed purely (`catch_up_set`) and then requested once per name.
pub async fn catch_up(
    agents: &AgentsHandle,
    up: &[AgentName],
    dormant: Option<&(dyn Fn(&AgentName) -> bool + Send + Sync)>,
    fiber: bough_kernel::FiberUid,
) -> Result<(), String> {
    let mut roster: Vec<(AgentName, usize)> = Vec::new();
    for name in up {
        // §1: a dormant agent gets NO wakes, and a catch-up is a wake. The loop's
        // `agent/wake-request` admission point is what enforces that, and `request_wake` below
        // goes through it — so a caller that HAS a `DormancyHandle` may pass it to skip the
        // request entirely, and one that does not is still correct as long as the listener is
        // registered.
        //
        // OPEN ITEM (recorded in `docs/phase-5-plan.md` §7.3): the row does not DECLARE the
        // dependency, so registration order still decides. Declaring it optional was tried and
        // reverted: an optional key that arrives after activation changes the committed view and
        // reloads the row, which re-raises the whole roster and leaves the disposed lanes on the
        // strip beside the new ones. Fixing it properly needs an activation handshake this phase
        // does not have.
        if dormant.is_some_and(|is_dormant| is_dormant(name)) {
            continue;
        }
        let Some(agent) = agents.by_name(name) else {
            continue;
        };
        let queued = agent
            .inbox()
            .pending(bough_plugin_agents::Target::NextWake)
            .len()
            + agent
                .inbox()
                .pending(bough_plugin_agents::Target::NextStep)
                .len();
        roster.push((name.clone(), queued));
    }

    for name in catch_up_set(&roster) {
        let Some(agent) = agents.by_name(&name) else {
            continue;
        };
        let req = agent
            .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
            .await;
        invariant::record(invariant::Obs {
            fiber,
            agent: name.clone(),
            started: matches!(req, WakeRequest::Started(_)),
        });
    }
    Ok(())
}

/// Register the roster as an effect: its inverse disposes every agent the row raised and forgets
/// this fiber's invariant stream. `apply` and the disposal test both go through it, so what the
/// test tears down is what the row registers.
pub async fn hold_roster(
    ctx: &Context,
    roster: Arc<Roster>,
    fiber: bough_kernel::FiberUid,
) -> Result<bough_kernel::EffectHandle, PluginError> {
    ctx.effect(move |e| async move {
        e.defer(move || async move { roster.dispose_all().await });
        e.defer_sync(move || invariant::forget(fiber));
        Ok(())
    })
    .await
}

/// The row.
pub struct ResidentsPlugin;

#[async_trait::async_trait]
impl Plugin for ResidentsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ResidentsConfig;

    fn inject() -> Inject {
        Inject::required(["agents", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        // A bootstrap name that is blank or carries a `/` would mint a trajectory id that is not
        // the `traj_prefix + name` the row documents, and §5's lane naming is what the projection
        // and every script address an agent by.
        for name in &cfg.bootstrap {
            if name.trim().is_empty() {
                return reject("bootstrap names must not be blank".to_string());
            }
            if name.contains('/') {
                return reject(format!("bootstrap name `{name}` must not contain `/`"));
            }
        }
        if cfg.traj_prefix.trim().is_empty() {
            return reject("traj_prefix must not be blank".to_string());
        }
        if cfg.catch_up && !cfg.resume_all {
            return reject(
                "catch_up requires resume_all: there is no roster to wake otherwise".to_string(),
            );
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        // The roster is an EFFECT: unloading this row disposes every agent it raised and leaves
        // the ledger untouched (P3-D17).
        let roster = Arc::new(Roster::default());
        let mine = ctx.fiber_uid();
        hold_roster(&ctx, Arc::clone(&roster), mine).await?;

        // The live half of a structural op. Registered BEFORE the roster is raised, so an op that
        // lands during boot is not missed.
        let agents2 = (*agents).clone();
        let ledger2 = (*ledger).clone();
        let roster2 = Arc::clone(&roster);
        ctx.on::<bough_plugin_agents::AgentRowsChanged, _, _>(move |changed| {
            let (agents, ledger, roster) = (agents2.clone(), ledger2.clone(), Arc::clone(&roster2));
            async move {
                if let Err(detail) = reconcile_rows(&agents, &ledger, &roster, &changed).await {
                    tracing::error!(target: "residents", "reconciling rows: {detail}");
                }
            }
        })
        .await?;

        // The factory slot may not be filled yet (row order carries no load semantics), so the
        // work runs AFTER `apply` returns, as an effect the row's disposal halts.
        let cfg = Arc::clone(&cfg);
        ctx.effect_spawn(move |ectx| async move {
            let entry = ectx.ctx().entry_id().clone();
            let kernel = ectx.ctx().kernel();
            let run = async {
                wait_for_factory(&agents).await?;
                let up = raise_roster(&agents, &ledger, &cfg, &roster).await?;
                if cfg.catch_up {
                    // The activation handshake `catch_up`'s OPEN ITEM asks for. A catch-up is a
                    // wake, and §1's "a dormant agent costs nothing" is enforced by a LISTENER on
                    // `agent/wake-request` that another row (`dormancy`) registers in its own
                    // `apply`; with no listener the admission point defaults to OPEN. Row order
                    // carries no load semantics, so whether that listener exists yet when this
                    // task reaches here is a race — one the unoptimized binary LOST every time
                    // (`scripts/tui/12-many-agents.sh`: a dormant lane ran its catch-up wake).
                    // The kernel's quiesce is the tree-wide "every row that will activate has":
                    // wait for it, then ask. On the ceiling (`false`) the tree is not settling
                    // and the launcher is already treating boot as failed; asking anyway is what
                    // this row did before, so nothing regresses.
                    if let Some(k) = &kernel {
                        let _ = k.quiesce().await;
                    }
                    catch_up(&agents, &up, None, mine).await?;
                }
                Ok::<(), String>(())
            };
            match run.await {
                Ok(()) => Ok(()),
                Err(detail) => {
                    tracing::error!(target: "residents", "{detail}");
                    Err(PluginError::new(entry, anyhow::anyhow!(detail)))
                }
            }
        });
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(ResidentsPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catch_up_set_skips_an_agent_with_nothing_queued() {
        let roster = vec![
            (AgentName::new("sol"), 2),
            (AgentName::new("terra"), 0),
            (AgentName::new("luna"), 1),
        ];
        assert_eq!(
            catch_up_set(&roster),
            vec![AgentName::new("sol"), AgentName::new("luna")]
        );
    }

    #[test]
    fn catch_up_set_is_empty_when_nothing_is_queued() {
        assert!(catch_up_set(&[(AgentName::new("sol"), 0)]).is_empty());
        assert!(catch_up_set(&[]).is_empty());
    }
}
