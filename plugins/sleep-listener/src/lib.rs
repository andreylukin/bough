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

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};

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

/// The row.
pub struct SleepListenerPlugin;

#[async_trait::async_trait]
impl Plugin for SleepListenerPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = SleepListenerConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::none()
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-8: `min_sleep_ms > 0`")
    }

    /// Start the platform source on its own thread, provide `power`, and defer the teardown that
    /// stops the run loop and joins the thread. WP-8.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-8")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(SleepListenerPlugin);
