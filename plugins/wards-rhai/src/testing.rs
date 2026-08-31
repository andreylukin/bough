//! Invariant: this row exists so the ward host can be mounted in a TREE without WP-1's scheduler
//! Providers. It registers nothing and fires nothing — a ward's `schedule` action against it is
//! refused, which is exactly what a tree with no scheduler should do.
//!
//! Like `ledger-memory` and `agent-loop-scripted`: in the binary's catalog, in NO bundle. A test's
//! own `--patch` mounts it.

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError};
use bough_plugin_schedule::{
    JobInfo, JobName, JobRun, JobSpec, Schedule, ScheduleError, ScheduleHandle, Scheduler,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "schedule-null";

/// A Scheduler that holds nothing.
pub struct NullScheduler;

#[async_trait::async_trait]
impl Scheduler for NullScheduler {
    fn provider(&self) -> &'static str {
        PLUGIN_NAME
    }
    async fn register(&self, _ctx: &Context, spec: JobSpec) -> Result<EffectHandle, PluginError> {
        Err(PluginError::new(
            bough_kernel::EntryId::new(PLUGIN_NAME),
            anyhow::anyhow!("`{}` registers no jobs", spec.name),
        ))
    }
    fn jobs(&self) -> Vec<JobInfo> {
        Vec::new()
    }
    async fn fire_now(&self, name: &JobName) -> Result<JobRun, ScheduleError> {
        Err(ScheduleError::Unknown(name.clone()))
    }
}

/// The row.
pub struct NullSchedulePlugin;

#[async_trait::async_trait]
impl Plugin for NullSchedulePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = NullScheduleConfig;

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        ctx.provide::<Schedule>(ScheduleHandle(Arc::new(NullScheduler)))
            .await
            .map_err(|e| PluginError::new(entry, e))?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

/// No knobs.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct NullScheduleConfig {}

bough_kernel::register_plugin!(NullSchedulePlugin);
