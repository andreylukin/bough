//! Invariant: these seven are ORDINARY tools. Nothing here reaches around the `tools` seam — each
//! is registered through `ToolsHandle::register` and guarded by the same pipeline — and the row
//! is mounted for BOTH consumers, so the bench compares SURFACES and not tool inventories.

pub mod bg;
pub mod clock;
pub mod files;
pub mod inbox;
pub mod invariant;
pub mod ledger_read;
pub mod schedule;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};

pub use clock::{Clock, SystemClock};
pub use schedule::{ScheduleFiredBody, ScheduleId, ScheduleIntentBody};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tools-operator";

/// What this row registers. `view`/`patch`/`write` are the file verbs; `bg`, `ledger_read`,
/// `inbox` and `schedule` are the four the sandbox surface sugars.
pub const TOOL_NAMES: [&str; 7] = [
    "view",
    "patch",
    "write",
    "bg",
    "ledger_read",
    "inbox",
    "schedule",
];

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OperatorConfig {
    pub max_view_bytes: usize,
    pub max_files_per_patch: usize,
    pub bg_log_dir: PathBuf,
    pub bg_max: usize,
    pub bg_poll_ms: u64,
    pub ledger_page: usize,
    pub schedule_max_horizon_days: u32,
    pub schedule_tick_ms: u64,
}

/// The Consumer row.
pub struct OperatorPlugin;

#[async_trait::async_trait]
impl Plugin for OperatorPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = OperatorConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "ledger", "workspace"])
            .union(&bough_kernel::Inject::optional(["agents", "mail", "schedule"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.max_view_bytes == 0
            || cfg.max_files_per_patch == 0
            || cfg.bg_max == 0
            || cfg.bg_poll_ms == 0
            || cfg.ledger_page == 0
            || cfg.schedule_tick_ms == 0
        {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "every bound must be at least 1".to_string(),
            });
        }
        Ok(())
    }

    /// Registers all seven specs as effects, declares the two `schedule/*` step types, starts the
    /// due-watcher, and kills every live `bg` job on disposal.
    ///
    /// WP-4 owns the body.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4: register the seven specs, the step types, and the schedule watcher")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::every_fire_names_a_live_intent()]
    }
}

bough_kernel::register_plugin!(OperatorPlugin);

/// A branded id is a plain string in a body schema; `brand_id!` lives in `bough-util`, which has
/// no `schemars` dependency, so the impls are written here — the same shape every id in the tree
/// already has.
macro_rules! id_json_schema {
    ($($t:ty),* $(,)?) => {$(
        impl schemars::JsonSchema for $t {
            fn schema_name() -> std::borrow::Cow<'static, str> {
                std::borrow::Cow::Borrowed(stringify!($t))
            }
            fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
                schemars::json_schema!({ "type": "string" })
            }
        }
    )*};
}

id_json_schema!(schedule::ScheduleId, bg::BgId);
