//! Invariant: this crate is the power SERVICE DEFINITION (§13, §0.2). It owns the `power` key, the
//! two events and the source contract — and no FFI.
//!
//! `power/changed` is PARALLEL, not EMIT: a catch-up wake is durable work, `emit` is spawned and
//! unawaited (P2-D25), and nothing durable may ride one.

pub mod invariant;

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{
    ConfigError, Context, InvariantSpec, ParallelEvent, Plugin, PluginError, ServiceKey,
};
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "power";

/// The `power` service key.
pub struct Power;

impl ServiceKey for Power {
    type Value = PowerHandle;
    const NAME: &'static str = "power";
}

/// What the machine did.
#[derive(Clone, Debug, PartialEq)]
pub enum PowerEvent {
    WillSleep {
        at: DateTime<Utc>,
    },
    DidWake {
        at: DateTime<Utc>,
        /// `None` when the source cannot say (NSWorkspace's fallback path).
        asleep_for: Option<Duration>,
    },
}

/// `power/changed` — PARALLEL.
pub struct PowerChanged;

impl ParallelEvent for PowerChanged {
    const NAME: &'static str = "power/changed";
    type Payload = PowerEvent;
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct PowerHandle(pub Arc<dyn PowerSource>);

/// What a power Provider does.
pub trait PowerSource: Send + Sync + 'static {
    /// `"iokit"` | `"nsworkspace"` | `"noop"` | `"test"`. The swap test reads it.
    fn kind(&self) -> &'static str;
    /// The last event this source saw, if any.
    fn last(&self) -> Option<PowerEvent>;
}

/// No configuration: the sources belong to the Provider rows.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PowerConfig {}

/// The Service Definition row.
pub struct PowerPlugin;

#[async_trait::async_trait]
impl Plugin for PowerPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PowerConfig;

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-8: declare the seam; a Provider provides `power`")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(PowerPlugin);
