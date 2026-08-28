//! §0.2 runtime invariant for `bough-plugin-tui-timeline`:
//!
//! **The rendered row set is a subset of the queried step set, and is strictly non-decreasing in
//! `(at, traj, seq)`.** A timeline that invented a row, or that reordered two rows, is the one
//! way this pane can lie: everything else it shows is a step's own field. Both halves are checked
//! over what the LAST FRAME actually painted, against the step ids that frame's query returned.
//!
//! The recorder is a crate-level slot rather than a service binding, for the same reason
//! `tui-search`'s is: there is one terminal, `render` is synchronous, and the check must not have
//! to reach back into a pane that may already be disposed.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::StepId;
use parking_lot::Mutex;

use crate::Row;

const NAME: &str = "rendered_rows_are_a_subset_of_the_query_in_order";

/// What the last frame painted, and the step ids the query behind it returned.
static LAST: Mutex<Option<(Vec<Row>, Vec<StepId>)>> = Mutex::new(None);

/// Record a frame. Called from `Pane::render`; allocation-only, no I/O.
pub fn record(rendered: &[Row], queried: &[StepId]) {
    *LAST.lock() = Some((rendered.to_vec(), queried.to_vec()));
}

/// The last recorded frame.
pub fn last() -> Option<(Vec<Row>, Vec<StepId>)> {
    LAST.lock().clone()
}

/// Forget the recorded frame. The row's disposal path: a disabled row leaves nothing behind.
pub fn forget() {
    *LAST.lock() = None;
}

/// PURE: the check.
pub fn check(rendered: &[Row], queried: &[StepId]) -> Result<(), String> {
    for row in rendered {
        if !queried.contains(&row.step.id) {
            return Err(format!(
                "the timeline rendered step `{}` (agent `{}`, kind `{}`), which the query behind \
                 the frame never returned",
                row.step.id, row.agent, row.step.kind
            ));
        }
    }
    for pair in rendered.windows(2) {
        let (a, b) = (&pair[0], &pair[1]);
        let key = |r: &Row| (r.step.at, r.traj.clone(), r.step.seq);
        if key(a) > key(b) {
            return Err(format!(
                "the timeline rendered step `{}` ({}, {}/{}) before step `{}` ({}, {}/{}), which \
                 is out of the (at, traj, seq) order the timeline is defined by",
                a.step.id,
                a.step.at.to_rfc3339(),
                a.traj,
                a.step.seq.0,
                b.step.id,
                b.step.at.to_rfc3339(),
                b.traj,
                b.step.seq.0,
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
    let Some((rendered, queried)) = last() else {
        return Ok(());
    };
    check(&rendered, &queried).map_err(|detail| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::row;

    #[test]
    fn a_rendered_row_that_is_not_in_the_queried_set_is_reported() {
        let a = row("sol", "t1", 1, "x", "12:00:00");
        let b = row("terra", "t2", 1, "x", "12:00:01");
        let err = check(&[a.clone(), b.clone()], std::slice::from_ref(&a.step.id))
            .expect_err("`b` was never queried");
        assert!(err.contains(b.step.id.as_str()), "{err}");
        assert!(err.contains("never returned"), "{err}");
    }

    #[test]
    fn an_out_of_order_render_is_reported() {
        let a = row("sol", "t1", 1, "x", "12:00:05");
        let b = row("terra", "t2", 1, "x", "12:00:01");
        let ids = vec![a.step.id.clone(), b.step.id.clone()];
        let err = check(&[a, b], &ids).expect_err("newest first is not the timeline's order");
        assert!(err.contains("out of the (at, traj, seq) order"), "{err}");
    }

    #[test]
    fn a_clean_render_passes() {
        let a = row("sol", "t1", 1, "x", "12:00:00");
        let b = row("terra", "t2", 1, "x", "12:00:01");
        // A tie on `at` broken the right way is clean too.
        let c = row("terra", "t2", 2, "x", "12:00:01");
        let ids = vec![a.step.id.clone(), b.step.id.clone(), c.step.id.clone()];
        assert!(check(&[a, b, c], &ids).is_ok());
    }
}
