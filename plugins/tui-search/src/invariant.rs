//! §0.2 runtime invariant for `bough-plugin-tui-search`:
//!
//! **Every hit row rendered names a step that still exists in the ledger.** A search pane is the
//! easiest place to show a fact that is no longer there; the check is over the pane's own rendered
//! rows against the ledger it queried.
//!
//! The recorder is a crate-level slot rather than a service binding on purpose: there is exactly
//! one terminal and therefore exactly one search pane, and `render` is synchronous, so the check
//! must not have to reach back into a pane that may already be disposed.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, StepId};
use parking_lot::Mutex;

const NAME: &str = "every_rendered_hit_names_a_live_step";

static LAST_RENDERED: Mutex<Vec<crate::HitRow>> = Mutex::new(Vec::new());

/// What the last frame put on screen. Called from `Pane::render`; allocation-only, no I/O.
pub fn record(rows: &[crate::HitRow]) {
    *LAST_RENDERED.lock() = rows.to_vec();
}

/// The rows the last frame painted.
pub fn rendered() -> Vec<crate::HitRow> {
    LAST_RENDERED.lock().clone()
}

/// Forget the recorded frame. The row's disposal path: a disabled row leaves nothing behind.
pub fn forget() {
    LAST_RENDERED.lock().clear();
}

/// PURE: the check, over the rendered rows and the step ids the ledger still holds.
pub fn check_rows(rendered: &[crate::HitRow], known: &[StepId]) -> Result<(), String> {
    for row in rendered {
        if !known.contains(&row.step) {
            return Err(format!(
                "the search pane rendered a hit naming step `{}` (traj `{}`, seq {}), which the \
                 ledger no longer holds",
                row.step, row.traj, row.seq.0
            ));
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let rows = rendered();
    if rows.is_empty() {
        return Ok(());
    }
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        // The row is being torn down; there is nothing to state about a ledger that is gone.
        return Ok(());
    };
    let mut known: Vec<StepId> = Vec::with_capacity(rows.len());
    for row in &rows {
        if ledger
            .0
            .step(&row.step)
            .await
            .map_err(|e| fail(e.to_string()))?
            .is_some()
        {
            known.push(row.step.clone());
        }
    }
    check_rows(&rows, &known).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{AgentName, Seq, StepType, TrajId};

    fn row(step: &str) -> crate::HitRow {
        crate::HitRow {
            agent: Some(AgentName::new("sol")),
            traj: TrajId::new("lane/sol"),
            step: StepId::new(step),
            seq: Seq(3),
            kind: StepType::new("thought/text"),
            snippet: "the swap gate".into(),
        }
    }

    #[test]
    fn a_rendered_hit_whose_step_is_gone_is_a_violation() {
        let rendered = vec![row("s1"), row("s2")];
        let known = vec![StepId::new("s1")];
        let err = check_rows(&rendered, &known).expect_err("`s2` is gone");
        assert!(err.contains("s2"), "{err}");
    }

    #[test]
    fn every_rendered_hit_present_in_the_ledger_passes() {
        let rendered = vec![row("s1")];
        assert!(check_rows(&rendered, &[StepId::new("s1")]).is_ok());
    }
}
