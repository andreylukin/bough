//! Invariant: this Provider fires ONLY when a test (or `/power` in a dev profile) tells it to.
//! There is no timer and no platform hook, so a synthetic `WillSleep`/`DidWake` pair is the whole
//! of the event stream and a wake test needs no laptop (P6-D1).
//!
//! In the catalog, in NO bundle.

pub mod invariant;

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{ConfigError, Context, Inject, InvariantSpec, Plugin, PluginError};
use bough_plugin_commands::{
    Command, CommandCx, CommandError, CommandOutput, CommandScope, CommandSpec, Commands,
    Invocation, OutputRender,
};
use bough_plugin_power::{Power, PowerEvent, PowerHandle, PowerSource};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "power-test";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PowerTestConfig {
    /// Register a `/power sleep|wake` command when `commands` is present.
    #[serde(default)]
    pub command: bool,
}

/// The synthetic source, plus the half that fires it.
#[derive(Clone)]
pub struct PowerTestHandle {
    ctx: Context,
    last: Arc<parking_lot::Mutex<Option<PowerEvent>>>,
}

impl PowerTestHandle {
    /// A source bound to `ctx` — the context its dispatches ride.
    pub fn new(ctx: Context) -> PowerTestHandle {
        PowerTestHandle {
            ctx,
            last: Arc::new(parking_lot::Mutex::new(None)),
        }
    }

    /// Dispatch a synthetic event through `power/changed`, AWAITED (it is a parallel event).
    ///
    /// `last` is written BEFORE the dispatch, so a listener that reads the seam back during its
    /// own handler sees the event it is handling — and so the seam's invariant can never catch
    /// this source mid-update.
    pub async fn fire(&self, ev: PowerEvent) {
        *self.last.lock() = Some(ev.clone());
        bough_plugin_power::dispatch(&self.ctx, ev).await;
    }

    /// A `WillSleep` now.
    pub async fn sleep(&self) {
        self.fire(PowerEvent::WillSleep {
            at: chrono::Utc::now(),
        })
        .await;
    }

    /// A `DidWake` now, `asleep_for` as given.
    pub async fn wake(&self, asleep_for: Option<Duration>) {
        self.fire(PowerEvent::DidWake {
            at: chrono::Utc::now(),
            asleep_for,
        })
        .await;
    }
}

impl PowerSource for PowerTestHandle {
    fn kind(&self) -> &'static str {
        "test"
    }
    fn last(&self) -> Option<PowerEvent> {
        self.last.lock().clone()
    }
}

/// `/power sleep|wake [seconds]`.
struct PowerCommand {
    handle: PowerTestHandle,
}

#[async_trait::async_trait]
impl Command for PowerCommand {
    async fn run(&self, inv: Invocation, _cx: CommandCx) -> Result<CommandOutput, CommandError> {
        let usage = "/power sleep|wake [asleep_seconds]".to_string();
        let what = inv.args.first().map(String::as_str).unwrap_or_default();
        match what {
            "sleep" => self.handle.sleep().await,
            "wake" => {
                let secs = match inv.args.get(1) {
                    None => None,
                    Some(s) => Some(s.parse::<u64>().map_err(|e| CommandError::BadArgs {
                        usage: usage.clone(),
                        detail: e.to_string(),
                    })?),
                };
                self.handle.wake(secs.map(Duration::from_secs)).await;
            }
            other => {
                return Err(CommandError::BadArgs {
                    usage,
                    detail: format!("unknown power event `{other}`"),
                })
            }
        }
        Ok(CommandOutput {
            text: format!("power: {what}"),
            render: OutputRender::Plain,
            cites: Vec::new(),
        })
    }
}

/// The test Provider row.
pub struct PowerTestPlugin;

#[async_trait::async_trait]
impl Plugin for PowerTestPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PowerTestConfig;

    fn inject() -> Inject {
        Inject::optional(["commands"])
    }

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let handle = PowerTestHandle::new(ctx.clone());
        ctx.provide::<Power>(PowerHandle(Arc::new(handle.clone()) as Arc<dyn PowerSource>))
            .await
            .map_err(|e| PluginError::new(entry.clone(), e))?;

        if cfg.command {
            let commands = ctx
                .try_get::<Commands>()
                .map_err(|e| PluginError::new(entry.clone(), e))?;
            if let Some(commands) = commands {
                commands
                    .register(
                        &ctx,
                        CommandSpec {
                            name: bough_plugin_commands::CommandName::new("power"),
                            // MERGE (track B -> ux1): NOT "fire a synthetic
                            // sleep or wake". Phase ux1's M16 rule refuses a
                            // user-facing summary that uses one of this tree's
                            // house words, and `wake` is one; the swap gate
                            // boots this row, so the sentence had to become
                            // plain English. The USAGE line still spells
                            // `wake`: that is the literal the command parses.
                            summary: "pretend the machine went to sleep or came back".to_string(),
                            usage: "/power sleep|wake [asleep_seconds]".to_string(),
                            args: bough_plugin_commands::positional(
                                &["event", "asleep_seconds"],
                                1,
                            ),
                            scope: CommandScope::Global,
                            run: Arc::new(PowerCommand {
                                handle: handle.clone(),
                            }),
                        },
                    )
                    .await?;
            }
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(PowerTestPlugin);

#[cfg(test)]
mod tests {
    use super::*;
    use bough_kernel::KernelCore;

    #[tokio::test]
    async fn firing_writes_last_before_it_dispatches() {
        let ctx = Context::root(KernelCore::new());
        let h = PowerTestHandle::new(ctx);
        assert_eq!(h.last(), None);
        assert_eq!(h.kind(), "test");
        h.sleep().await;
        assert!(matches!(h.last(), Some(PowerEvent::WillSleep { .. })));
        h.wake(Some(Duration::from_secs(300))).await;
        match h.last() {
            Some(PowerEvent::DidWake { asleep_for, .. }) => {
                assert_eq!(asleep_for, Some(Duration::from_secs(300)))
            }
            other => panic!("expected a wake, got {other:?}"),
        }
    }
}
