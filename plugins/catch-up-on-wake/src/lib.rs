//! Invariant: EXACTLY ONE catch-up wake per active agent per wake. `Agent::request_wake` already
//! returns `Nothing` when there is nothing queued, so "only over queued mail" falls out of the
//! seam; the half the seam does not give is the second `DidWake` arriving while a catch-up is still
//! in flight, and an `in_flight` set drops it here.
//!
//! A `DidWake` whose `asleep_for` is under `min_sleep_ms` produces none: a lid closed for ten
//! seconds is not a night away.

pub mod invariant;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::{
    Agent, AgentId, AgentKind, Agents, AgentsHandle, WakeCause, WakeKind, WakeRequest,
};
use bough_plugin_power::{PowerChanged, PowerEvent};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "catch-up-on-wake";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatchUpOnWakeConfig {
    pub min_sleep_ms: u64,
    /// Which agent kinds get a catch-up wake. `["resident"]`.
    pub kinds: Vec<String>,
}

/// The three kinds a `kinds` entry may name. A name outside this set is a boot failure, not a
/// silently-empty roster (§0.2).
pub const KNOWN_KINDS: [&str; 3] = ["resident", "worker", "fork"];

/// The YAML spelling of an [`AgentKind`].
pub fn kind_str(kind: AgentKind) -> &'static str {
    match kind {
        AgentKind::Resident => "resident",
        AgentKind::Worker => "worker",
        AgentKind::Fork => "fork",
    }
}

/// PURE: does this event ask for a catch-up at all?
///
/// A `WillSleep` never does. A `DidWake` does when the machine was away for at least
/// `min_sleep_ms` — and ALSO when the source cannot say how long (`asleep_for: None`, the
/// NSWorkspace fallback): a missed night is a worse failure than a redundant wake that
/// `request_wake` will answer `Nothing` to anyway.
pub fn asks_for_catch_up(ev: &PowerEvent, min_sleep_ms: u64) -> bool {
    match ev {
        PowerEvent::WillSleep { .. } => false,
        PowerEvent::DidWake { asleep_for, .. } => match asleep_for {
            None => true,
            Some(d) => *d >= Duration::from_millis(min_sleep_ms),
        },
    }
}

/// PURE: is this agent one of the kinds the row was configured for, and still alive?
pub fn eligible(agent: &Agent, kinds: &[String]) -> bool {
    !agent.is_disposed() && kinds.iter().any(|k| k == kind_str(agent.kind()))
}

/// The consumer's state: who is mid-catch-up.
pub struct CatchUpOnWake {
    cfg: Arc<CatchUpOnWakeConfig>,
    agents: AgentsHandle,
    in_flight: parking_lot::Mutex<HashSet<AgentId>>,
    fiber: bough_kernel::FiberUid,
}

impl CatchUpOnWake {
    pub fn new(
        cfg: Arc<CatchUpOnWakeConfig>,
        agents: AgentsHandle,
        fiber: bough_kernel::FiberUid,
    ) -> CatchUpOnWake {
        CatchUpOnWake {
            cfg,
            agents,
            in_flight: parking_lot::Mutex::new(HashSet::new()),
            fiber,
        }
    }

    /// The roster this row wakes.
    pub fn agents(&self) -> &AgentsHandle {
        &self.agents
    }

    /// Who is currently mid-catch-up. The test reads it rather than a count.
    pub fn in_flight(&self) -> Vec<AgentId> {
        let mut v: Vec<AgentId> = self.in_flight.lock().iter().cloned().collect();
        v.sort_by_key(|id| id.to_string());
        v
    }

    /// The catch-up this row started for `id` is over: the agent may be woken again.
    ///
    /// It is a method rather than a timer because the "in flight" window is exactly one wake, and
    /// only the thing that awaited the wake can say when it closed.
    pub fn finish(&self, id: &AgentId) {
        self.in_flight.lock().remove(id);
    }

    /// One `DidWake`: request a wake per eligible agent, skipping those already in flight.
    /// Returns whom it woke, so the test asserts on the set rather than on a count.
    pub async fn on_wake(&self, ev: &PowerEvent) -> Vec<AgentId> {
        if !asks_for_catch_up(ev, self.cfg.min_sleep_ms) {
            return Vec::new();
        }
        let mut woken = Vec::new();
        for agent in self.agents.list() {
            if !eligible(&agent, &self.cfg.kinds) {
                continue;
            }
            // Claim and request in one pass: the claim is what makes a second `DidWake` arriving
            // mid-catch-up a no-op rather than a second wake over the same mail.
            if !self.in_flight.lock().insert(agent.id().clone()) {
                continue;
            }
            let req = agent
                .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
                .await;
            invariant::record(invariant::Obs {
                fiber: self.fiber,
                agent: agent.id().clone(),
                event_at: ev.at(),
                started: matches!(req, WakeRequest::Started(_)),
            });
            match req {
                // Nothing was queued, so nothing is in flight and the next wake may ask again.
                WakeRequest::Nothing => {
                    self.finish(agent.id());
                }
                WakeRequest::Started(_) => woken.push(agent.id().clone()),
            }
        }
        woken
    }
}

/// Register the `power/changed` listener as an effect. `apply` and the wake test both go through
/// it, so what the test fires at is what the row registers.
pub async fn listen(
    ctx: &Context,
    state: Arc<CatchUpOnWake>,
) -> Result<bough_kernel::EffectHandle, PluginError> {
    ctx.on_parallel::<PowerChanged, _, _>(move |ev| {
        let state = Arc::clone(&state);
        async move {
            for id in state.on_wake(&ev).await {
                // The window closes when the wake this row opened goes idle. Held on the
                // listener's own task: the parallel dispatch is awaited by the Provider, and
                // waiting for the wake to FINISH here would make a sleep listener block on a
                // model.
                let state = Arc::clone(&state);
                tokio::spawn(async move {
                    if let Some(agent) = state.agents().get(&id) {
                        agent.when_idle().await;
                    }
                    state.finish(&id);
                });
            }
        }
    })
    .await
}

/// The row.
pub struct CatchUpOnWakePlugin;

#[async_trait::async_trait]
impl Plugin for CatchUpOnWakePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = CatchUpOnWakeConfig;

    fn inject() -> Inject {
        Inject::required(["power", "agents"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let reject = |detail: String| Err(ConfigError::Rejected { detail });
        if cfg.kinds.is_empty() {
            return reject(
                "kinds must not be empty: a row that can wake nobody is a misconfiguration, not a \
                 disabled row"
                    .to_string(),
            );
        }
        for k in &cfg.kinds {
            if !KNOWN_KINDS.contains(&k.as_str()) {
                return reject(format!(
                    "unknown agent kind `{k}`; expected one of {KNOWN_KINDS:?}"
                ));
            }
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let agents = ctx
            .get::<Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let fiber = ctx.fiber_uid();
        invariant::forget(fiber);
        let state = Arc::new(CatchUpOnWake::new(
            Arc::clone(&cfg),
            (*agents).clone(),
            fiber,
        ));

        listen(&ctx, Arc::clone(&state)).await?;

        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(fiber));
            Ok(())
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(CatchUpOnWakePlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn wake(secs: u64) -> PowerEvent {
        PowerEvent::DidWake {
            at: chrono::Utc::now(),
            asleep_for: Some(Duration::from_secs(secs)),
        }
    }

    #[test]
    fn a_sleep_never_asks_for_a_catch_up() {
        assert!(!asks_for_catch_up(
            &PowerEvent::WillSleep {
                at: chrono::Utc::now()
            },
            60_000
        ));
    }

    #[test]
    fn a_short_nap_asks_for_nothing_and_a_night_asks() {
        assert!(!asks_for_catch_up(&wake(10), 60_000));
        assert!(asks_for_catch_up(&wake(60), 60_000), "exactly at the floor");
        assert!(asks_for_catch_up(&wake(8 * 3600), 60_000));
    }

    #[test]
    fn a_source_that_cannot_say_how_long_still_asks() {
        let ev = PowerEvent::DidWake {
            at: chrono::Utc::now(),
            asleep_for: None,
        };
        assert!(asks_for_catch_up(&ev, 60_000));
    }

    #[test]
    fn the_kinds_field_is_validated_loudly() {
        let bad = CatchUpOnWakeConfig {
            min_sleep_ms: 1,
            kinds: vec![],
        };
        assert!(CatchUpOnWakePlugin::validate(&bad).is_err());
        let bad = CatchUpOnWakeConfig {
            min_sleep_ms: 1,
            kinds: vec!["residents".to_string()],
        };
        assert!(
            CatchUpOnWakePlugin::validate(&bad).is_err(),
            "a typo is a boot failure"
        );
        let good = CatchUpOnWakeConfig {
            min_sleep_ms: 1,
            kinds: vec!["resident".to_string()],
        };
        assert!(CatchUpOnWakePlugin::validate(&good).is_ok());
    }
}
