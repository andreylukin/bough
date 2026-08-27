//! Invariant: an intent fires EXACTLY ONCE. Every `schedule/fired` names a `schedule/intent` that
//! exists and was not already fired — including across a restart replay, which is why the intent
//! is a ledger step and not a timer in memory.
//!
//! This is §5's "own scheduled intents", which nothing exposes today. When Phase 7's
//! `ctx.schedule` lands, the due-watcher half is deleted and the tool registers a cron entry
//! instead; the handoff is written up in `docs/codemode-merge-notes.md`.

use std::sync::Arc;

use bough_plugin_agents::MessageId;
use bough_plugin_ledger::AgentName;
use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome};
use chrono::{DateTime, Utc};

use crate::clock::Clock;
use crate::OperatorConfig;

bough_util::brand_id!(
    /// One scheduled intent.
    pub struct ScheduleId;
);

/// `schedule/intent` — Evidence (it cites the step that asked for it).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ScheduleIntentBody {
    pub id: ScheduleId,
    pub agent: AgentName,
    pub at: DateTime<Utc>,
    pub intent: String,
}

/// `schedule/fired` — Thought.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ScheduleFiredBody {
    pub id: ScheduleId,
    pub at: DateTime<Utc>,
    pub message: MessageId,
}

/// The two step types this row owns, for `declare_step_types`.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    const OWNER: &str = crate::PLUGIN_NAME;
    vec![
        StepTypeDef::of::<ScheduleIntentBody>("schedule/intent", OWNER)
            .class_rule(ClassRule::Evidence),
        StepTypeDef::of::<ScheduleFiredBody>("schedule/fired", OWNER).class_rule(ClassRule::Thought),
    ]
}

/// `schedule` — takes `{at, intent}`.
pub struct Schedule {
    #[allow(dead_code)]
    pub cfg: Arc<OperatorConfig>,
    #[allow(dead_code)]
    pub clock: Arc<dyn Clock>,
}

#[async_trait::async_trait]
impl Tool for Schedule {
    /// WP-4 owns the body.
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-4: validate the horizon, append schedule/intent, hand it to the watcher")
    }
}

/// The due-watcher: at the due time it delivers a `Wake` message to the creator's next wake and
/// appends `schedule/fired`.
///
/// WP-4 owns the body.
pub async fn watch(
    _ctx: bough_kernel::Context,
    _cfg: Arc<OperatorConfig>,
    _clock: Arc<dyn Clock>,
) -> Result<(), bough_kernel::PluginError> {
    todo!("WP-4: tick on schedule_tick_ms, deliver through Agent::send, append schedule/fired")
}
