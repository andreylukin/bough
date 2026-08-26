//! Invariant: a hook failure is CONTAINED AND COUNTED, never retried inside the same dispatch
//! (§7). A non-zero exit, a timeout, unparseable stdout and stdout over `max_output_bytes` are ALL
//! ONE THING: a failure. After `max_failures` consecutive failures the POINT is QUARANTINED for the
//! life of the process (P6-D14) and is not invoked again; re-enabling it is a patch — the manual
//! off/on switch §7 itself names.
//!
//! P6-D13: hook points name ledger step types plus three harness points (`boot`, `schedule/fired`,
//! `power/changed`). §9 says "named hook points" and names none; step types are the names the rest
//! of the system already uses, so a hook point needs no second vocabulary.
//!
//! WHAT A HOOK RETURNS IS NOT WHAT A HOOK DOES. The executable returns [`RuntimeAction`]s and
//! performs none; the [`ActionSink`] carries them out, and the only production sink is
//! `runtime_actions::execute_all` — the one place a runtime script's intent meets the write
//! boundary (§9).

pub mod dispatch;
pub mod invariant;
pub mod vocabulary;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{Append, Class, Ledger, LedgerHandle, StepType, TrajId, WakeId};
use bough_plugin_runtime_actions::{
    ActionOutcome, RuntimeAction, RuntimeCx, RuntimeLimits, RuntimeSource, Trigger,
};

pub use dispatch::{run_hook, HookFailure};
pub use vocabulary::{HookFired, HOOK_FIRED};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "hooks-exec";

/// The three HARNESS points, alongside every ledger step type (P6-D13).
pub const HARNESS_POINTS: [&str; 3] = ["boot", "schedule/fired", "power/changed"];

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HooksConfig {
    pub points: Vec<HookPoint>,
    pub max_output_bytes: usize,
    /// Consecutive failures after which a point is QUARANTINED for the life of the process.
    pub max_failures: u32,
    pub limits: RuntimeLimits,
}

/// One hook point.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HookPoint {
    /// A ledger step type (`mail/delivered`) or a named harness point (`boot`, `schedule/fired`).
    pub point: String,
    pub exec: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// stdin: one JSON object, one line.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct HookInput {
    pub point: String,
    /// RFC 3339. The clock is injected by the dispatcher.
    pub at: String,
    pub event: serde_json::Value,
}

/// stdout: one JSON object. The whole protocol.
#[derive(
    Clone, Debug, PartialEq, Default, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct HookOutput {
    #[serde(default)]
    pub actions: Vec<RuntimeAction>,
    #[serde(default)]
    pub note: Option<String>,
}

/// Where a hook point stands.
#[derive(Clone, Debug, PartialEq)]
pub enum HookState {
    Ready,
    Failing { consecutive: u32, last: String },
    Quarantined { reason: String },
}

/// What carries out the actions a hook returned.
///
/// One implementation in production ([`RuntimeSink`], which is
/// `runtime_actions::execute_all`); a recording one in the tests, so "the returned actions are
/// journaled" is asserted on what reached the boundary rather than on the process's exit code.
#[async_trait::async_trait]
pub trait ActionSink: Send + Sync + 'static {
    async fn execute(
        &self,
        source: &RuntimeSource,
        trigger: &Trigger,
        actions: &[RuntimeAction],
        at: chrono::DateTime<chrono::Utc>,
    ) -> Vec<ActionOutcome>;
}

/// THE production sink: everything a hook returned goes through the one executor (§9).
pub struct RuntimeSink {
    pub cx: RuntimeCx,
    pub limits: RuntimeLimits,
}

#[async_trait::async_trait]
impl ActionSink for RuntimeSink {
    async fn execute(
        &self,
        source: &RuntimeSource,
        trigger: &Trigger,
        actions: &[RuntimeAction],
        at: chrono::DateTime<chrono::Utc>,
    ) -> Vec<ActionOutcome> {
        let mut cx = self.cx.clone();
        cx.source = source.clone();
        cx.trigger = trigger.clone();
        cx.at = at;
        bough_plugin_runtime_actions::execute_all(&cx, actions, &self.limits).await
    }
}

/// One point's live state and its exec counter.
struct PointState {
    point: HookPoint,
    state: parking_lot::Mutex<HookState>,
    execs: AtomicU64,
}

/// The live host: one state per point.
pub struct HooksHost {
    cfg: Arc<HooksConfig>,
    points: Vec<Arc<PointState>>,
}

/// What one dispatch did. Returned so the caller can journal it without re-deriving it.
#[derive(Clone, Debug, PartialEq)]
pub struct Fired {
    pub point: String,
    pub exec: String,
    pub actions: Vec<RuntimeAction>,
    pub outcomes: Vec<String>,
    pub ms: u64,
    pub ok: bool,
}

impl HooksHost {
    /// A host over a validated config.
    pub fn new(cfg: Arc<HooksConfig>) -> HooksHost {
        let points = cfg
            .points
            .iter()
            .map(|p| {
                Arc::new(PointState {
                    point: p.clone(),
                    state: parking_lot::Mutex::new(HookState::Ready),
                    execs: AtomicU64::new(0),
                })
            })
            .collect();
        HooksHost { cfg, points }
    }

    /// Every configured point and where it stands.
    pub fn hooks(&self) -> Vec<(String, PathBuf, HookState)> {
        self.points
            .iter()
            .map(|p| {
                (
                    p.point.point.clone(),
                    p.point.exec.clone(),
                    p.state.lock().clone(),
                )
            })
            .collect()
    }

    /// How many times this point's executable has actually been SPAWNED. The quarantine test reads
    /// it: "not invoked again" is a statement about processes, not about log lines.
    pub fn exec_count(&self, point: &str) -> u64 {
        self.points
            .iter()
            .filter(|p| p.point.point == point)
            .map(|p| p.execs.load(Ordering::SeqCst))
            .sum()
    }

    /// Run every executable bound to `point`: write [`HookInput`], read [`HookOutput`], count the
    /// failure or clear the streak. Bounded by `timeout_ms` and `max_output_bytes`.
    ///
    /// Returns what each one returned, CLAMPED by [`RuntimeLimits`] but not yet executed.
    pub async fn dispatch(
        &self,
        point: &str,
        event: serde_json::Value,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Vec<RuntimeAction> {
        let mut all = Vec::new();
        for ps in self.for_point(point) {
            let (actions, _) = self.run_one(&ps, point, event.clone(), at).await;
            all.extend(actions);
        }
        all
    }

    /// [`HooksHost::dispatch`] plus the boundary: everything returned goes through `sink`, and the
    /// result is one [`Fired`] per invocation, ready to become a `hook/fired` row.
    pub async fn fire(
        &self,
        point: &str,
        event: serde_json::Value,
        at: chrono::DateTime<chrono::Utc>,
        trigger: &Trigger,
        sink: &dyn ActionSink,
    ) -> Vec<Fired> {
        let mut fired = Vec::new();
        for ps in self.for_point(point) {
            let started = std::time::Instant::now();
            let (actions, ok) = self.run_one(&ps, point, event.clone(), at).await;
            if !ok && actions.is_empty() {
                // A quarantined or failed point still says so: a hook that did nothing because it
                // was never invoked and one that failed are both visible through `hooks()`.
                fired.push(Fired {
                    point: point.to_string(),
                    exec: ps.point.exec.display().to_string(),
                    actions: Vec::new(),
                    outcomes: Vec::new(),
                    ms: started.elapsed().as_millis() as u64,
                    ok: false,
                });
                continue;
            }
            let source = RuntimeSource::Hook(ps.point.point.clone());
            let outcomes = sink.execute(&source, trigger, &actions, at).await;
            fired.push(Fired {
                point: point.to_string(),
                exec: ps.point.exec.display().to_string(),
                actions,
                outcomes: outcomes.iter().map(render_outcome).collect(),
                ms: started.elapsed().as_millis() as u64,
                ok,
            });
        }
        fired
    }

    fn for_point(&self, point: &str) -> Vec<Arc<PointState>> {
        self.points
            .iter()
            .filter(|p| p.point.point == point)
            .cloned()
            .collect()
    }

    /// One invocation, with the whole failure policy in one place.
    async fn run_one(
        &self,
        ps: &Arc<PointState>,
        point: &str,
        event: serde_json::Value,
        at: chrono::DateTime<chrono::Utc>,
    ) -> (Vec<RuntimeAction>, bool) {
        if matches!(*ps.state.lock(), HookState::Quarantined { .. }) {
            // NOT INVOKED. The counter is the proof.
            return (Vec::new(), false);
        }
        let input = HookInput {
            point: point.to_string(),
            at: at.to_rfc3339(),
            event,
        };
        ps.execs.fetch_add(1, Ordering::SeqCst);
        match run_hook(&ps.point, &input, self.cfg.max_output_bytes).await {
            Ok(out) => {
                *ps.state.lock() = HookState::Ready;
                let (actions, dropped) =
                    bough_plugin_runtime_actions::clamp(&out.actions, &self.cfg.limits);
                for d in dropped {
                    tracing::warn!(point = %point, exec = %ps.point.exec.display(), dropped = %d,
                                   "hook action dropped by the host's limits");
                }
                (actions, true)
            }
            Err(err) => {
                self.count_failure(ps, point, &err);
                (Vec::new(), false)
            }
        }
    }

    /// The whole failure policy: count, report, quarantine at `max_failures`. NEVER retry inside
    /// this dispatch.
    fn count_failure(&self, ps: &Arc<PointState>, point: &str, err: &HookFailure) {
        let mut state = ps.state.lock();
        let consecutive = match &*state {
            HookState::Failing { consecutive, .. } => consecutive + 1,
            _ => 1,
        };
        if consecutive >= self.cfg.max_failures {
            let reason = format!("{consecutive} consecutive failures; last: {err}");
            tracing::warn!(point = %point, exec = %ps.point.exec.display(), reason = %reason,
                           "hook point QUARANTINED for the life of the process");
            *state = HookState::Quarantined { reason };
        } else {
            tracing::warn!(point = %point, exec = %ps.point.exec.display(), error = %err,
                           consecutive, "hook failed");
            *state = HookState::Failing {
                consecutive,
                last: err.to_string(),
            };
        }
    }
}

/// PURE: one line per action outcome, in order.
pub fn render_outcome(o: &ActionOutcome) -> String {
    match o {
        ActionOutcome::Did { detail } => format!("did: {detail}"),
        ActionOutcome::Refused { reason } => format!("refused: {reason}"),
    }
}

/// PURE: the `hook/fired` row one [`Fired`] becomes.
/// `trigger` is the step that caused the dispatch; the row CITES it, which is what makes "no point
/// is invoked more than once per dispatch" a relation the invariant module can check.
pub fn fired_step(
    traj: &TrajId,
    wake: &WakeId,
    f: &Fired,
    trigger: Option<&bough_plugin_ledger::StepId>,
    at: chrono::DateTime<chrono::Utc>,
) -> Append {
    Append {
        traj: traj.clone(),
        wake: wake.clone(),
        kind: StepType::new(HOOK_FIRED),
        class: Class::Thought,
        body: serde_json::to_value(HookFired {
            point: f.point.clone(),
            exec: f.exec.clone(),
            actions: f.actions.clone(),
            outcomes: f.outcomes.clone(),
            ms: f.ms,
            ok: f.ok,
        })
        .unwrap_or(serde_json::Value::Null),
        cites: trigger
            .map(|t| {
                vec![bough_plugin_ledger::Cite {
                    r#ref: bough_plugin_ledger::Ref::new(format!("step:{t}")),
                    url: None,
                }]
            })
            .unwrap_or_default(),
        at,
        id: None,
    }
}

/// The row.
pub struct HooksExecPlugin;

#[async_trait::async_trait]
impl Plugin for HooksExecPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = HooksConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["ledger", "agents", "actions", "workers", "schedule"])
            .union(&bough_kernel::Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let bad = |detail: String| ConfigError::Rejected { detail };
        if cfg.max_failures == 0 {
            return Err(bad(
                "max_failures: zero would quarantine every point before its first run".into(),
            ));
        }
        if cfg.max_output_bytes == 0 {
            return Err(bad(
                "max_output_bytes: zero means no hook could ever return an action".into(),
            ));
        }
        for p in &cfg.points {
            if p.point.trim().is_empty() {
                return Err(bad("point: a hook point needs a name".into()));
            }
            if !p.exec.is_absolute() {
                return Err(bad(format!(
                    "point `{}`: `exec` must be an absolute path, not `{}` — a hook must not \
                     resolve through PATH",
                    p.point,
                    p.exec.display()
                )));
            }
            if p.timeout_ms == 0 {
                return Err(bad(format!(
                    "point `{}`: timeout_ms must be greater than zero",
                    p.point
                )));
            }
        }
        Ok(())
    }

    /// Subscribe once per distinct point; on a fire, `dispatch` then the sink, then append ONE
    /// `hook/fired` per invocation.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());
        ledger
            .declare_step_types(&ctx, vocabulary::step_types())
            .await?;

        let agents = ctx
            .get::<bough_plugin_agents::Agents>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let workers = ctx
            .get::<bough_plugin_workers::Workers>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let actions = ctx
            .get::<bough_plugin_actions::Actions>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let schedule = ctx
            .get::<bough_plugin_schedule::Schedule>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        let host = Arc::new(HooksHost::new(cfg.clone()));
        let sink: Arc<dyn ActionSink> = Arc::new(RuntimeSink {
            cx: RuntimeCx {
                ctx: ctx.clone(),
                agents: bough_plugin_agents::AgentsHandle(agents.0.clone()),
                ledger: ledger.clone(),
                workers: bough_plugin_workers::WorkersHandle(workers.0.clone()),
                actions: bough_plugin_actions::ActionsHandle(actions.0.clone()),
                schedule: bough_plugin_schedule::ScheduleHandle(schedule.0.clone()),
                // Replaced per invocation with the point that fired.
                source: RuntimeSource::Hook(String::new()),
                trigger: Trigger::synthetic(&RuntimeSource::Hook(String::new())),
                at: chrono::Utc::now(),
            },
            limits: cfg.limits.clone(),
        });

        // The `boot` harness point, fired once, right here. Its actions have no triggering step,
        // so the trigger is synthetic (`Trigger::synthetic`) and the `hook/fired` row cites
        // nothing — there is nothing to cite.
        //
        // The other two harness points (`schedule/fired`, `power/changed`) are NOT wired: their
        // events belong to rows this work package does not own. See `docs/track-b-merge-notes.md`.
        {
            let h = Arc::clone(&host);
            let s = Arc::clone(&sink);
            let now = chrono::Utc::now();
            let trigger = bough_plugin_runtime_actions::Trigger::synthetic(&RuntimeSource::Hook(
                "boot".to_string(),
            ));
            for f in h
                .fire("boot", serde_json::json!({}), now, &trigger, s.as_ref())
                .await
            {
                tracing::info!(point = %f.point, exec = %f.exec, ok = f.ok, "boot hook fired");
            }
        }

        // ONE listener for every ledger-step point: `ledger/step` is the ledger's only event, and
        // a consumer that wants "on mail delivered" filters by `kind` (see `ledger/src/events.rs`).
        let h = Arc::clone(&host);
        let s = Arc::clone(&sink);
        let l = ledger.clone();
        ctx.on::<bough_plugin_ledger::LedgerStep, _, _>(move |step| {
            let (h, s, l) = (Arc::clone(&h), Arc::clone(&s), l.clone());
            async move {
                let point = step.kind.as_str().to_string();
                if h.for_point(&point).is_empty() {
                    return;
                }
                let event = serde_json::json!({
                    "step": step.id.as_str(),
                    "traj": step.traj.as_str(),
                    "wake": step.wake.as_str(),
                    "kind": step.kind.as_str(),
                    "body": &*step.body,
                });
                let trigger = Trigger {
                    agent: None,
                    wake: step.wake.clone(),
                    step: step.id.clone(),
                };
                for f in h.fire(&point, event, step.at, &trigger, s.as_ref()).await {
                    let row = fired_step(&step.traj, &step.wake, &f, Some(&step.id), step.at);
                    if let Err(e) = l.0.append(row).await {
                        tracing::warn!(point = %f.point, error = %e, "`hook/fired` did not append");
                    }
                }
            }
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(HooksExecPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(points: Vec<HookPoint>) -> HooksConfig {
        HooksConfig {
            points,
            max_output_bytes: 65536,
            max_failures: 3,
            limits: RuntimeLimits {
                max_actions: 16,
                max_spawns: 2,
                max_text_bytes: 8192,
            },
        }
    }

    fn point(name: &str) -> HookPoint {
        HookPoint {
            point: name.into(),
            exec: PathBuf::from("/bin/true"),
            args: vec![],
            timeout_ms: 1000,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn validate_refuses_a_relative_exec() {
        let mut p = point("boot");
        p.exec = PathBuf::from("hook.sh");
        let err = HooksExecPlugin::validate(&cfg(vec![p])).expect_err("refused");
        assert!(format!("{err}").contains("absolute"), "{err}");
    }

    #[test]
    fn validate_refuses_the_settings_that_could_never_work() {
        assert!(HooksExecPlugin::validate(&HooksConfig {
            max_failures: 0,
            ..cfg(vec![])
        })
        .is_err());
        assert!(HooksExecPlugin::validate(&HooksConfig {
            max_output_bytes: 0,
            ..cfg(vec![])
        })
        .is_err());
        let mut p = point("boot");
        p.timeout_ms = 0;
        assert!(HooksExecPlugin::validate(&cfg(vec![p])).is_err());
        assert!(HooksExecPlugin::validate(&cfg(vec![point("boot")])).is_ok());
    }

    #[test]
    fn a_fresh_host_reports_every_point_ready() {
        let host = HooksHost::new(Arc::new(cfg(vec![point("boot"), point("mail/delivered")])));
        let hooks = host.hooks();
        assert_eq!(hooks.len(), 2);
        assert!(hooks.iter().all(|(_, _, s)| *s == HookState::Ready));
        assert_eq!(host.exec_count("boot"), 0);
    }
}
