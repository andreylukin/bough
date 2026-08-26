//! Invariant: a hook failure is CONTAINED AND COUNTED, never retried inside the same dispatch
//! (§7). A non-zero exit, a timeout, unparseable stdout and stdout over `max_output_bytes` are ALL
//! ONE THING: a failure. After `max_failures` consecutive failures the POINT is QUARANTINED for the
//! life of the process (P6-D14) and is not invoked again; re-enabling it is a patch — the manual
//! off/on switch §7 itself names.
//!
//! P6-D13: hook points name ledger step types plus three harness points (`boot`, `schedule/fired`,
//! `power/changed`). §9 says "named hook points" and names none; step types are the names the rest
//! of the system already uses, so a hook point needs no second vocabulary.

pub mod invariant;
pub mod vocabulary;

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_runtime_actions::{RuntimeAction, RuntimeLimits};

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

/// The live host: one state per point.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct HooksHost {
    cfg: Arc<HooksConfig>,
    state: parking_lot::Mutex<Vec<(String, PathBuf, HookState)>>,
}

impl HooksHost {
    /// Every configured point and where it stands. WP-7.
    pub fn hooks(&self) -> Vec<(String, PathBuf, HookState)> {
        todo!("WP-7")
    }

    /// Run one point's executable: write [`HookInput`], read [`HookOutput`], count the failure or
    /// clear the streak. Bounded by `timeout_ms` and `max_output_bytes`. WP-7.
    pub async fn dispatch(
        &self,
        point: &str,
        event: serde_json::Value,
        at: chrono::DateTime<chrono::Utc>,
    ) -> Vec<RuntimeAction> {
        let _ = (point, event, at);
        todo!("WP-7")
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
        let _ = cfg;
        todo!("WP-7: `max_failures > 0`, `max_output_bytes > 0`, an absolute `exec`, non-zero timeouts")
    }

    /// Subscribe once per distinct point; on a fire, `dispatch` then
    /// `runtime_actions::execute_all`, then append ONE `hook/fired`. WP-7.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-7")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(HooksExecPlugin);
