//! Invariant (§2): the about-line has TWO HALVES and they are never confused. The STATE half
//! cites the steps it summarises and is evidence; the INTENT half is rendered under an explicit
//! "intent (self-declared)" label and is never presented as truth. The line is refreshed on
//! COMPLETED wakes only — a preempted wake refreshes nothing (§5).
//!
//! P2-D11: the refresh is this plugin's own `about/line` step, appended on the `agent/wake-end`
//! moment. A plugin writing into another plugin's step body would break the ledger's ownership
//! rule (§3), so the MOMENT is shared, not the row.

pub mod compose;
pub mod invariant;
pub mod section;

use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_agents::AgentWakeEnd;
use bough_plugin_ledger::vocabulary::WakeEndReason;
use bough_plugin_ledger::{
    Append, Class, ClassRule, Ledger, LedgerHandle, Order, StepQuery, StepType, StepTypeDef, WakeId,
};
use bough_plugin_projection::{DropPriority, Projection, SectionScope, SectionSpec};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "about-line";

/// The step type this crate owns, spelled once.
pub const ABOUT_LINE: &str = "about/line";

/// The label the INTENT half is rendered under. §2: never as truth.
pub const INTENT_LABEL: &str = "intent (self-declared)";

/// `about/line` — EVIDENCE. Cites are the steps the STATE half summarises.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct AboutLine {
    /// What is true, cited.
    pub state: String,
    /// What the agent says it means to do next. SELF-DECLARED, never truth.
    pub intent: String,
    pub of_wake: WakeId,
}

/// The step type this crate owns.
pub fn step_types() -> Vec<StepTypeDef> {
    vec![
        // EVIDENCE, so the ledger itself refuses a line with no cites: "the state half cites the
        // steps it summarises" is enforced at append, not only by the invariant module.
        StepTypeDef::of::<AboutLine>(ABOUT_LINE, PLUGIN_NAME).class_rule(ClassRule::Evidence),
    ]
}

/// Render the newest line as a projection section body: the state half, then the intent half
/// under its explicit label. Pure, so the labelling is a unit test rather than a screenshot.
pub fn render(line: &AboutLine) -> String {
    let mut out = String::new();
    out.push_str(line.state.trim());
    if !line.intent.trim().is_empty() {
        out.push_str("\n\n");
        out.push_str(INTENT_LABEL);
        out.push_str(": ");
        out.push_str(line.intent.trim());
    }
    out
}

/// The row's config: the two lengths a deployment might want to tune.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AboutConfig {
    pub max_state_chars: usize,
    pub max_intent_chars: usize,
}

/// The newest `about/line` on a trajectory, as the section renderer and the tests both read it.
pub async fn newest(
    ledger: &LedgerHandle,
    traj: &bough_plugin_ledger::TrajId,
) -> Result<Option<bough_plugin_ledger::Step>, bough_plugin_ledger::LedgerError> {
    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            kinds: vec![StepType::new(ABOUT_LINE)],
            order: Order::SeqDesc,
            limit: Some(1),
            ..Default::default()
        })
        .await?;
    Ok(steps.into_iter().next())
}

/// Append the refresh for one wake.
///
/// `Ok(None)` for any reason but `Completed`: §5's "a preempted wake refreshes nothing" is a
/// property of THIS function, not only of which wakes `agent/wake-end` happens to dispatch for,
/// so a second Provider of the wake seam cannot re-open the hole by dispatching more widely.
/// Pulled out of the listener so a test can drive the whole durable path without a scheduler.
pub async fn refresh(
    ledger: &LedgerHandle,
    cfg: &AboutConfig,
    wake: &WakeId,
    reason: WakeEndReason,
    end_step: &bough_plugin_ledger::StepId,
) -> Result<Option<bough_plugin_ledger::Step>, anyhow::Error> {
    if reason != WakeEndReason::Completed {
        return Ok(None);
    }
    let end = ledger
        .0
        .step(end_step)
        .await?
        .ok_or_else(|| anyhow::anyhow!("`wake/end` step `{end_step}` is not in the ledger"))?;
    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![end.traj.clone()],
            wake: Some(wake.clone()),
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?;
    let composed = compose::compose(&steps, wake, end_step, cfg);
    let step = ledger
        .0
        .append(Append {
            traj: end.traj.clone(),
            wake: wake.clone(),
            kind: StepType::new(ABOUT_LINE),
            class: Class::Evidence,
            body: serde_json::to_value(&composed.line)?,
            cites: compose::cites_of(&composed.cites),
            at: end.at,
            id: None,
        })
        .await?;
    Ok(Some(step))
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

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let ledger = LedgerHandle(ledger.0.clone());
        ledger.declare_step_types(&ctx, step_types()).await?;

        let projection = ctx
            .get::<Projection>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        bough_plugin_projection::ProjectionHandle(projection.0.clone())
            .section(
                &ctx,
                SectionSpec {
                    id: section::section_id(),
                    position: section::POSITION,
                    scope: SectionScope::Global,
                    agent: None,
                    // An answer wake must always be buildable, and the line is what the agent
                    // knows about itself: it is never a degradation rung's first casualty (§5).
                    priority: DropPriority::Never,
                    render: Arc::new(section::AboutSection),
                },
            )
            .await?;

        // The refresh: COMPLETED wakes only. `agent/wake-end` is dispatched for completed wakes,
        // and this listener checks the reason anyway — a preempted wake refreshing the line would
        // be a lie about what the agent is doing, so the check lives on both sides.
        let l = ledger.clone();
        ctx.on_parallel::<AgentWakeEnd, _, _>(move |ended| {
            let l = l.clone();
            let cfg = cfg.clone();
            async move {
                if let Err(e) = refresh(&l, &cfg, &ended.wake, ended.reason, &ended.end_step).await
                {
                    // A refresh that cannot be written is reported, never swallowed and never
                    // fatal: the identity band degrades to the previous line.
                    tracing::warn!(wake = %ended.wake, error = %e, "about-line refresh failed");
                }
            }
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::lines_cite_and_follow_completed_wakes()]
    }
}

bough_kernel::register_plugin!(AboutLinePlugin);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_intent_half_renders_under_its_explicit_label() {
        let line = AboutLine {
            state: "read the plan; ran `bash`".into(),
            intent: "write the tests next".into(),
            of_wake: WakeId::new("w1"),
        };
        assert_eq!(
            render(&line),
            "read the plan; ran `bash`\n\nintent (self-declared): write the tests next"
        );
    }

    #[test]
    fn a_line_with_no_intent_renders_no_label() {
        let line = AboutLine {
            state: "nothing to report".into(),
            intent: String::new(),
            of_wake: WakeId::new("w1"),
        };
        assert_eq!(render(&line), "nothing to report");
    }
}
