//! Invariant (§2): the about-line has TWO HALVES and they are never confused. The STATE half
//! cites the steps it summarises and is evidence; the INTENT half is rendered under an explicit
//! "intent (self-declared)" label and is never presented as truth. The line is refreshed on
//! COMPLETED wakes only — a preempted wake refreshes nothing (§5).
//!
//! P2-D11: the refresh is this plugin's own `about/line` step, appended on the `agent/wake-end`
//! moment. A plugin writing into another plugin's step body would break the ledger's ownership
//! rule (§3), so the MOMENT is shared, not the row.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{StepTypeDef, WakeId};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "about-line";

/// `about/line` — EVIDENCE. Cites are the steps the STATE half summarises.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AboutLine {
    /// What is true, cited.
    pub state: String,
    /// What the agent says it means to do next. SELF-DECLARED, never truth.
    pub intent: String,
    pub of_wake: WakeId,
}

/// The step type this crate owns. WP-5.
pub fn step_types() -> Vec<StepTypeDef> {
    todo!("WP-5: about/line, Evidence")
}

/// Render the newest line as a projection section body: the state half, then the intent half
/// under its explicit label. Pure, so the labelling is a unit test rather than a screenshot.
///
/// WP-5.
pub fn render(_line: &AboutLine) -> String {
    todo!("WP-5: state, then `intent (self-declared): ...`")
}

/// The row's config: the two lengths a deployment might want to tune.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AboutConfig {
    pub max_state_chars: usize,
    pub max_intent_chars: usize,
}

/// The consumer row.
pub struct AboutLinePlugin;

#[async_trait::async_trait]
impl Plugin for AboutLinePlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = AboutConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["agents", "ledger", "projection"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: declare about/line, listen on agent/wake-end, contribute the Identity/After section")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::lines_cite_and_follow_completed_wakes()]
    }
}

bough_kernel::register_plugin!(AboutLinePlugin);
