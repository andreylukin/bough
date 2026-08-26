//! Invariant: at most ONE catch-up wake per agent per activation, and none at all for an agent
//! with nothing queued (§5, V6). TUI launch is the lid-open proxy (§13: there is no lid
//! notification on macOS; Phase 7's `sleep-listener` row will call the same method).
//!
//! The row holds every resumed agent's `AgentDisposer` inside its own effect, so disabling
//! `residents` by patch tears the roster down and leaves the ledger untouched (P3-D17).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Inject, InvariantSpec, Plugin, PluginError};
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
    let deadline = std::time::Instant::now() + FACTORY_WAIT;
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
    let mut up: Vec<AgentName> = Vec::new();

    for name in &cfg.bootstrap {
        let name = AgentName::new(name);
        let existing = ledger.0.agent(&name).await.map_err(|e| e.to_string())?;
        if existing.is_some() {
            continue;
        }
        let traj = TrajId::new(format!("{}{}", cfg.traj_prefix, name));
        let (agent, disposer) = agents
            .create(CreateAgent {
                name: name.clone(),
                traj,
                kind: AgentKind::Resident,
                scope: None,
                setup: None,
                seed: Vec::new(),
                at: now,
            })
            .await
            .map_err(|e| e.to_string())?;
        roster.push(disposer);
        up.push(agent.name().clone());
    }

    if cfg.resume_all {
        for row in ledger.0.agents().await.map_err(|e| e.to_string())? {
            if agents.by_name(&row.name).is_some() {
                continue;
            }
            let (agent, disposer) = agents
                .resume(ResumeAgent {
                    name: row.name.clone(),
                    at: now,
                    setup: None,
                })
                .await
                .map_err(|e| e.to_string())?;
            roster.push(disposer);
            up.push(agent.name().clone());
        }
    }
    Ok(up)
}

/// §5's catch-up: ONE wake per agent that has queued mail, and nothing at all for one that does
/// not. The set is computed purely (`catch_up_set`) and then requested once per name.
pub async fn catch_up(
    agents: &AgentsHandle,
    up: &[AgentName],
    fiber: bough_kernel::FiberUid,
) -> Result<(), String> {
    let mut roster: Vec<(AgentName, usize)> = Vec::new();
    for name in up {
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

        // The factory slot may not be filled yet (row order carries no load semantics), so the
        // work runs AFTER `apply` returns, as an effect the row's disposal halts.
        let cfg = Arc::clone(&cfg);
        ctx.effect_spawn(move |ectx| async move {
            let entry = ectx.ctx().entry_id().clone();
            let run = async {
                wait_for_factory(&agents).await?;
                let up = raise_roster(&agents, &ledger, &cfg, &roster).await?;
                if cfg.catch_up {
                    catch_up(&agents, &up, mine).await?;
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
