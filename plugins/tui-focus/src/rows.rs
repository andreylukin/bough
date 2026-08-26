//! Invariant: the projection of a trajectory into rows is PURE and TOTAL. `tool/call` and
//! `tool/result` fold into ONE row by call id; envelope steps fold into their neighbours or are
//! dropped; and an UNKNOWN step type renders as `Other` and never panics — the step-type map is
//! merge-extensible (§3), so a renderer will meet types it does not own.

use bough_plugin_agents::Phase;
use bough_plugin_ledger::vocabulary::MailClass;
use bough_plugin_ledger::{Step, StepId, StepType, WakeId};
use bough_plugin_llm::ToolCallId;
use bough_plugin_tools::{RenderIntent, ToolResultBody};
use bough_plugin_tui_render::AboutView;

/// One rendered row of a trajectory.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Mail {
        step: StepId,
        from: String,
        subject: String,
        class: MailClass,
    },
    Andrey {
        step: StepId,
        text: String,
    },
    Text {
        step: StepId,
        wake: WakeId,
        index: u32,
        text: String,
    },
    Reasoning {
        step: StepId,
        text: String,
    },
    Tool {
        call: ToolCallId,
        name: String,
        intent: RenderIntent,
        args: serde_json::Value,
        result: Option<ToolResultBody>,
        call_step: StepId,
    },
    WakeMark {
        step: StepId,
        wake: WakeId,
        phase: Phase,
        reason: Option<String>,
    },
    About {
        step: StepId,
        view: AboutView,
    },
    Other {
        step: StepId,
        kind: StepType,
    },
}

/// PURE: the whole projection of a trajectory into rows. `tool/call` and `tool/result` fold into
/// ONE [`Row::Tool`] by call id; envelope steps (`step/start`, `request/header`, `inbox/spliced`)
/// fold into their neighbours or are dropped. Unit-tested against a fixture step list.
pub fn rows_from_steps(_steps: &[Step]) -> Vec<Row> {
    todo!("WP-4")
}
