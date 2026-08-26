//! Invariant: neither system pass invents behaviour. `schedule-catch-up` makes exactly the call
//! `catch-up-on-wake` makes (`Agent::request_wake(WakeKind::Catchup, WakeCause::CatchUp)`), so
//! "one catch-up per active agent" has ONE implementation; and `schedule-reconsolidate` resolves
//! its command BY NAME through `ctx.commands` and never reaches into Phase 4's code.
//!
//! P6-D2: a command that does not exist yet makes the JOB return [`JobOutcome::Pending`]. The ROW
//! stays ACTIVE — §0.2 makes an enabled row that never activates a boot failure, so a row that
//! stayed PENDING would break the boot instead of waiting politely.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::AgentsHandle;
use bough_plugin_commands::CommandsHandle;
use bough_plugin_schedule::{Cadence, Job, JobFire, JobOutcome};

/// The catch-up row's catalog name.
pub const CATCH_UP_NAME: &str = "schedule-catch-up";
/// The reconsolidation row's catalog name.
pub const RECONSOLIDATE_NAME: &str = "schedule-reconsolidate";

/// The catch-up pass's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CatchUpConfig {
    pub cadence: Cadence,
    pub catch_up: bool,
    /// Which agent kinds get a catch-up wake. `["resident"]`.
    pub kinds: Vec<String>,
}

/// The reconsolidation pass's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReconsolidateConfig {
    pub cadence: Cadence,
    pub catch_up: bool,
    /// The command to invoke, BY NAME, through `ctx.commands`. Absent command ⇒ the job returns
    /// [`JobOutcome::Pending`], the row stays ACTIVE, and the next cadence tries again (P6-D2).
    pub command: String,
    /// Whose lane the command runs in, when it needs one.
    pub agent: Option<String>,
}

/// The catch-up job: one `request_wake` per agent of a configured kind that is not disposed.
pub struct CatchUpJob {
    pub agents: AgentsHandle,
    pub kinds: Vec<String>,
}

#[async_trait::async_trait]
impl Job for CatchUpJob {
    async fn run(&self, fire: JobFire) -> JobOutcome {
        let _ = fire;
        todo!("WP-1: one request_wake per live agent of a configured kind; `Ran`")
    }
}

/// The reconsolidation job: resolve `command` through `ctx.commands` and dispatch it.
pub struct ReconsolidateJob {
    pub commands: Option<CommandsHandle>,
    pub agents: AgentsHandle,
    pub command: String,
    pub agent: Option<String>,
}

#[async_trait::async_trait]
impl Job for ReconsolidateJob {
    async fn run(&self, fire: JobFire) -> JobOutcome {
        let _ = fire;
        todo!("WP-1: `Pending` while the command does not exist (P6-D2)")
    }
}

/// The catch-up row.
pub struct CatchUpPlugin;

#[async_trait::async_trait]
impl Plugin for CatchUpPlugin {
    const NAME: &'static str = CATCH_UP_NAME;
    type Config = CatchUpConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["schedule", "agents"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-1: `cadence.check()`, non-empty `kinds`")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: register ONE job on ctx.schedule as an effect")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

/// The reconsolidation row.
pub struct ReconsolidatePlugin;

#[async_trait::async_trait]
impl Plugin for ReconsolidatePlugin {
    const NAME: &'static str = RECONSOLIDATE_NAME;
    type Config = ReconsolidateConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["schedule", "agents"])
            .union(&bough_kernel::Inject::optional(["commands"]))
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-1: `cadence.check()`, non-empty `command`")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: register ONE job on ctx.schedule as an effect")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(CatchUpPlugin);
bough_kernel::register_plugin!(ReconsolidatePlugin);
