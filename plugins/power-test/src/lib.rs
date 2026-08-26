//! Invariant: this Provider fires ONLY when a test (or `/power` in a dev profile) tells it to.
//! There is no timer and no platform hook, so a synthetic `WillSleep`/`DidWake` pair is the whole
//! of the event stream and a wake test needs no laptop (P6-D1).
//!
//! In the catalog, in NO bundle.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_power::{PowerEvent, PowerSource};

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
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct PowerTestHandle {
    ctx: Context,
    last: Arc<parking_lot::Mutex<Option<PowerEvent>>>,
}

impl PowerTestHandle {
    /// Dispatch a synthetic event through `power/changed`, AWAITED (it is a parallel event). WP-8.
    pub async fn fire(&self, ev: PowerEvent) {
        let _ = ev;
        todo!("WP-8")
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

/// The test Provider row.
pub struct PowerTestPlugin;

#[async_trait::async_trait]
impl Plugin for PowerTestPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PowerTestConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::optional(["commands"])
    }

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-8: provide `power` with a PowerTestHandle")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(PowerTestPlugin);
