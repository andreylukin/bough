//! Invariant: THE ROW ALWAYS ACTIVATES. §0.2 makes an enabled row that never activates a boot
//! failure, so "not macOS" may not mean "does not activate": on every non-macOS platform this row
//! provides a NO-OP source and says so in its `kind()`.
//!
//! On macOS, `IORegisterForSystemPower` is PRIMARY and runs on ITS OWN THREAD with a `CFRunLoop`
//! (crossterm's event loop cannot host one — §13). `kIOMessageSystemWillSleep` →
//! `IOAllowPowerChange` IMMEDIATELY, then `WillSleep`; `kIOMessageSystemHasPoweredOn` → `DidWake`.
//! NSWorkspace is the FALLBACK, used only when `IORegisterForSystemPower` returns a null port:
//! dark wakes produce no NSWorkspace notification at all, which is why IOKit is primary.

pub mod invariant;
#[cfg(target_os = "macos")]
pub mod macos;
pub mod noop;

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_power::{Power, PowerEvent, PowerHandle, PowerSource};
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "sleep-listener";

/// Which source to use.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// IOKit on macOS, no-op elsewhere.
    Auto,
    Iokit,
    Nsworkspace,
    Noop,
}

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SleepListenerConfig {
    pub enabled: bool,
    /// A sleep shorter than this produces no `DidWake` worth acting on.
    pub min_sleep_ms: u64,
    pub source: Source,
}

/// What [`choose`] decided. Separated from doing it so the platform rule is testable on any
/// platform, including the one that has no IOKit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Choice {
    Iokit,
    NsWorkspace,
    Noop,
    /// The configuration asks for a source this platform does not have. A boot failure, named:
    /// a row that quietly did nothing instead is exactly what §0.2 refuses.
    Refuse(&'static str),
}

/// PURE: which source this configuration means on this platform.
///
/// `auto` on a platform without IOKit is `Noop` and NOT a refusal — the row still activates, which
/// is the whole reason [`noop::NoopSource`] exists. An EXPLICIT `iokit`/`nsworkspace` off macOS is
/// a refusal, because a named source that silently became something else is a lie.
pub fn choose(source: Source, enabled: bool, is_macos: bool) -> Choice {
    if !enabled {
        return Choice::Noop;
    }
    match (source, is_macos) {
        (Source::Noop, _) => Choice::Noop,
        (Source::Auto, true) => Choice::Iokit,
        (Source::Auto, false) => Choice::Noop,
        (Source::Iokit, true) => Choice::Iokit,
        (Source::Nsworkspace, true) => Choice::NsWorkspace,
        (Source::Iokit, false) | (Source::Nsworkspace, false) => {
            Choice::Refuse("this source exists only on macOS; use `auto` or `noop`")
        }
    }
}

/// PURE: is this wake worth telling anyone about?
///
/// A wake the source cannot time (`None`) always is: the alternative is silently skipping a night
/// away because the fallback path could not measure it.
pub fn worth_dispatching(asleep_for: Option<Duration>, min_sleep_ms: u64) -> bool {
    match asleep_for {
        None => true,
        Some(d) => d >= Duration::from_millis(min_sleep_ms),
    }
}

/// PURE: how long the machine was away, given the `WillSleep` this source saw. `None` when it saw
/// none — the process started while the machine was already asleep, or the sleep was a dark one.
pub fn asleep_for(slept_at: Option<DateTime<Utc>>, woke_at: DateTime<Utc>) -> Option<Duration> {
    let slept = slept_at?;
    (woke_at - slept).to_std().ok()
}

/// The half of a source that is the same whatever the platform hook is: remember the `WillSleep`,
/// measure the wake, drop a wake too short to act on, and write `last` BEFORE dispatching.
///
/// It is one struct rather than a rule repeated in each source because the seam's invariant is
/// "`last()` is the last thing dispatched", and two copies of that rule is how one of them drifts.
pub struct Gate {
    min_sleep_ms: u64,
    last: parking_lot::Mutex<Option<PowerEvent>>,
    slept_at: parking_lot::Mutex<Option<DateTime<Utc>>>,
    sink: Arc<dyn Fn(PowerEvent) + Send + Sync>,
}

impl Gate {
    pub fn new(min_sleep_ms: u64, sink: Arc<dyn Fn(PowerEvent) + Send + Sync>) -> Arc<Gate> {
        Arc::new(Gate {
            min_sleep_ms,
            last: parking_lot::Mutex::new(None),
            slept_at: parking_lot::Mutex::new(None),
            sink,
        })
    }

    /// The last event that went out. Never an event that was dropped.
    pub fn last(&self) -> Option<PowerEvent> {
        self.last.lock().clone()
    }

    /// The platform saw a sleep.
    pub fn will_sleep(&self, at: DateTime<Utc>) {
        *self.slept_at.lock() = Some(at);
        *self.last.lock() = Some(PowerEvent::WillSleep { at });
        (self.sink)(PowerEvent::WillSleep { at });
    }

    /// The platform saw a wake. Returns whether it was dispatched.
    pub fn did_wake(&self, at: DateTime<Utc>) -> bool {
        let slept = self.slept_at.lock().take();
        let for_ = asleep_for(slept, at);
        if !worth_dispatching(for_, self.min_sleep_ms) {
            return false;
        }
        let ev = PowerEvent::DidWake {
            at,
            asleep_for: for_,
        };
        *self.last.lock() = Some(ev.clone());
        (self.sink)(ev);
        true
    }
}

/// The source every platform hook is wrapped in.
pub struct GatedSource {
    kind: &'static str,
    gate: Arc<Gate>,
    /// The platform half, held so dropping the source tears the hook down.
    _hook: Box<dyn std::any::Any + Send + Sync>,
}

impl GatedSource {
    pub fn new(
        kind: &'static str,
        gate: Arc<Gate>,
        hook: Box<dyn std::any::Any + Send + Sync>,
    ) -> GatedSource {
        GatedSource {
            kind,
            gate,
            _hook: hook,
        }
    }
}

impl PowerSource for GatedSource {
    fn kind(&self) -> &'static str {
        self.kind
    }
    fn last(&self) -> Option<PowerEvent> {
        self.gate.last()
    }
}

/// The sink a mounted row hands its platform hook: dispatch through the seam, on the runtime the
/// row was applied on. The hook's callback runs on a CFRunLoop thread with no runtime of its own,
/// so the handle is captured here rather than looked up there.
pub fn seam_sink(ctx: Context) -> Arc<dyn Fn(PowerEvent) + Send + Sync> {
    let handle = tokio::runtime::Handle::current();
    Arc::new(move |ev: PowerEvent| {
        let ctx = ctx.clone();
        handle.spawn(async move {
            bough_plugin_power::dispatch(&ctx, ev).await;
        });
    })
}

/// The row.
pub struct SleepListenerPlugin;

#[async_trait::async_trait]
impl Plugin for SleepListenerPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SleepListenerConfig;

    fn inject() -> Inject {
        Inject::none()
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        if cfg.min_sleep_ms == 0 {
            return Err(ConfigError::Rejected {
                detail: "min_sleep_ms must be > 0: a floor of zero makes every dark wake a \
                         catch-up"
                    .to_string(),
            });
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let choice = choose(cfg.source, cfg.enabled, cfg!(target_os = "macos"));
        if let Choice::Refuse(why) = choice {
            return Err(PluginError::new(
                entry,
                anyhow::anyhow!("source `{:?}`: {why}", cfg.source),
            ));
        }
        let gate = Gate::new(cfg.min_sleep_ms, seam_sink(ctx.clone()));
        let source = start(choice, Arc::clone(&gate), &entry)?;
        invariant::record(invariant::Obs {
            fiber: ctx.fiber_uid(),
            kind: source.kind(),
        });
        let fiber = ctx.fiber_uid();
        ctx.provide::<Power>(PowerHandle(Arc::new(source) as Arc<dyn PowerSource>))
            .await
            .map_err(|e| PluginError::new(entry, e))?;
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

/// Start the chosen platform hook. A macOS hook that will not start is NOT a boot failure when the
/// choice was `auto`: it degrades to the no-op source and says so once, because a laptop that
/// refuses to boot bough over a power notification is a worse outcome than a laptop without
/// catch-up-on-wake (§13: TUI-launch catch-up is the reliable baseline anyway).
fn start(
    choice: Choice,
    gate: Arc<Gate>,
    entry: &bough_kernel::EntryId,
) -> Result<GatedSource, PluginError> {
    match choice {
        Choice::Noop | Choice::Refuse(_) => Ok(GatedSource::new(
            "noop",
            gate,
            Box::new(noop::NoopSource) as Box<dyn std::any::Any + Send + Sync>,
        )),
        #[cfg(target_os = "macos")]
        Choice::Iokit => match macos::IokitSource::start(Arc::clone(&gate)) {
            Ok(hook) => Ok(GatedSource::new("iokit", gate, Box::new(hook))),
            Err(e) => {
                tracing::warn!(target: "sleep-listener", "IOKit gave no port ({e}); trying NSWorkspace");
                match macos::NsWorkspaceSource::start(Arc::clone(&gate)) {
                    Ok(hook) => Ok(GatedSource::new("nsworkspace", gate, Box::new(hook))),
                    Err(e2) => {
                        tracing::warn!(target: "sleep-listener", "NSWorkspace refused too ({e2}); no power notifications on this machine");
                        Ok(GatedSource::new("noop", gate, Box::new(noop::NoopSource)))
                    }
                }
            }
        },
        #[cfg(target_os = "macos")]
        Choice::NsWorkspace => match macos::NsWorkspaceSource::start(Arc::clone(&gate)) {
            Ok(hook) => Ok(GatedSource::new("nsworkspace", gate, Box::new(hook))),
            Err(e) => Err(PluginError::new(
                entry.clone(),
                anyhow::anyhow!("NSWorkspace: {e}"),
            )),
        },
        #[cfg(not(target_os = "macos"))]
        Choice::Iokit | Choice::NsWorkspace => {
            let _ = entry;
            unreachable!("`choose` refuses a macOS source off macOS")
        }
    }
}

bough_kernel::register_plugin!(SleepListenerPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;

    #[test]
    fn auto_is_iokit_on_macos_and_a_no_op_everywhere_else() {
        assert_eq!(choose(Source::Auto, true, true), Choice::Iokit);
        assert_eq!(
            choose(Source::Auto, true, false),
            Choice::Noop,
            "the row still ACTIVATES off macOS (§0.2)"
        );
    }

    #[test]
    fn a_named_macos_source_off_macos_is_refused_loudly() {
        assert!(matches!(
            choose(Source::Iokit, true, false),
            Choice::Refuse(_)
        ));
        assert!(matches!(
            choose(Source::Nsworkspace, true, false),
            Choice::Refuse(_)
        ));
    }

    #[test]
    fn disabled_is_a_no_op_source_not_an_absent_row() {
        assert_eq!(choose(Source::Iokit, false, true), Choice::Noop);
    }

    #[test]
    fn min_sleep_ms_of_zero_is_rejected() {
        let cfg = SleepListenerConfig {
            enabled: true,
            min_sleep_ms: 0,
            source: Source::Auto,
        };
        assert!(SleepListenerPlugin::validate(&cfg).is_err());
    }

    fn gate(min_ms: u64) -> (Arc<Gate>, Arc<Mutex<Vec<PowerEvent>>>) {
        let out = Arc::new(Mutex::new(Vec::new()));
        let sink_out = Arc::clone(&out);
        let g = Gate::new(
            min_ms,
            Arc::new(move |ev| sink_out.lock().push(ev)) as Arc<dyn Fn(PowerEvent) + Send + Sync>,
        );
        (g, out)
    }

    #[test]
    fn a_pair_measures_the_sleep_and_dispatches_both() {
        let (g, out) = gate(1);
        let t0 = Utc::now();
        g.will_sleep(t0);
        assert!(g.did_wake(t0 + chrono::Duration::seconds(120)));
        let seen = out.lock().clone();
        assert_eq!(seen.len(), 2);
        match &seen[1] {
            PowerEvent::DidWake { asleep_for, .. } => {
                assert_eq!(*asleep_for, Some(Duration::from_secs(120)))
            }
            other => panic!("expected a wake, got {other:?}"),
        }
        assert_eq!(g.last(), Some(seen[1].clone()), "last() is what went out");
    }

    #[test]
    fn a_wake_under_the_floor_is_dropped_and_does_not_move_last() {
        let (g, out) = gate(60_000);
        let t0 = Utc::now();
        g.will_sleep(t0);
        assert!(!g.did_wake(t0 + chrono::Duration::seconds(5)));
        assert_eq!(out.lock().len(), 1, "only the sleep went out");
        assert!(
            matches!(g.last(), Some(PowerEvent::WillSleep { .. })),
            "a dropped wake never becomes `last()` — the seam's invariant would catch it"
        );
    }

    #[test]
    fn a_wake_with_no_sleep_behind_it_still_goes_out() {
        let (g, out) = gate(60_000);
        assert!(g.did_wake(Utc::now()), "a dark wake is still a wake");
        assert_eq!(out.lock().len(), 1);
        let first = out.lock()[0].clone();
        match first {
            PowerEvent::DidWake { asleep_for, .. } => assert_eq!(asleep_for, None),
            _ => panic!("expected a wake"),
        }
    }

    #[test]
    fn asleep_for_is_none_without_a_sleep_and_never_negative() {
        let t0 = Utc::now();
        assert_eq!(asleep_for(None, t0), None);
        assert_eq!(
            asleep_for(Some(t0), t0 + chrono::Duration::seconds(3)),
            Some(Duration::from_secs(3))
        );
        assert_eq!(
            asleep_for(Some(t0), t0 - chrono::Duration::seconds(3)),
            None,
            "a clock that went backwards is not a negative sleep"
        );
    }
}
