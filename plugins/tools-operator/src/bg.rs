//! Invariant: a background job is owned by this row. `bg_max` bounds how many can be live, and
//! disposing the row kills every one of them — unwind leaves no orphan process.

use std::sync::Arc;

use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};

use crate::OperatorConfig;

bough_util::brand_id!(
    /// One background job.
    pub struct BgId;
);

/// One three-op tool — `{op: "start"|"output"|"kill"}` — sugared in JS as `bg(name, cmd)` /
/// `bg.output(id)` / `bg.kill(id)`.
pub struct Bg {
    #[allow(dead_code)]
    pub cfg: Arc<OperatorConfig>,
}

/// A live job, as `bg.output` reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct Job {
    pub id: BgId,
    pub name: String,
    pub cmd: String,
    pub pid: Option<u32>,
    pub exit: Option<i32>,
    /// Where the tee'd output lives: `bg_log_dir/<id>.log`.
    pub log: std::path::PathBuf,
}

#[async_trait::async_trait]
impl Tool for Bg {
    /// WP-4 owns the body.
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-4: start | output | kill over a detached child tee'd to bg_log_dir")
    }
}
