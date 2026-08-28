//! Invariant: this crate is the power SERVICE DEFINITION (§13, §0.2). It owns the `power` key, the
//! two events and the source contract — and no FFI.
//!
//! `power/changed` is PARALLEL, not EMIT: a catch-up wake is durable work, `emit` is spawned and
//! unawaited (P2-D25), and nothing durable may ride one.

pub mod invariant;

use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{
    ConfigError, Context, Inject, InvariantSpec, ParallelEvent, Plugin, PluginError, ServiceKey,
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

impl PowerEvent {
    /// When the machine did it.
    pub fn at(&self) -> DateTime<Utc> {
        match self {
            PowerEvent::WillSleep { at } => *at,
            PowerEvent::DidWake { at, .. } => *at,
        }
    }
    /// `"will-sleep"` | `"did-wake"`. For a log line and for the invariant's detail.
    pub fn kind(&self) -> &'static str {
        match self {
            PowerEvent::WillSleep { .. } => "will-sleep",
            PowerEvent::DidWake { .. } => "did-wake",
        }
    }
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

/// The dispatch every Provider goes through: RECORD the payload, then dispatch it, AWAITED.
///
/// It lives here and not in each Provider so the seam's invariant sees one stream whatever source
/// is mounted, and so a Provider cannot dispatch without recording.
pub async fn dispatch(ctx: &Context, ev: PowerEvent) {
    invariant::record(invariant::Obs {
        fiber: ctx.fiber_uid(),
        event: ev.clone(),
    });
    ctx.parallel::<PowerChanged>(ev).await;
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

    /// The Definition reads the key it defines for ONE reason: its invariant compares the stream
    /// it recorded against the mounted source's `last()`. It is optional because the Definition
    /// boots before any Provider does.
    fn inject() -> Inject {
        Inject::optional(["power"])
    }

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        // A reload keeps the FiberUid, so the record is cleared rather than accumulated.
        let fiber = ctx.fiber_uid();
        invariant::forget(fiber);
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

bough_kernel::register_plugin!(PowerPlugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_wake_carries_its_own_timestamp_and_kind() {
        let at = Utc::now();
        let ev = PowerEvent::DidWake {
            at,
            asleep_for: Some(Duration::from_secs(90)),
        };
        assert_eq!(ev.at(), at);
        assert_eq!(ev.kind(), "did-wake");
        assert_eq!(PowerEvent::WillSleep { at }.kind(), "will-sleep");
    }
}
