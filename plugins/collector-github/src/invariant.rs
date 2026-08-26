//! §0.2 runtime invariant for `bough-plugin-collector-github`:
//!
//! **No two `mail/delivered` steps on one trajectory CITE the same `gh:` ref.** That is the
//! at-least-once ref guard checked against the ledger rather than documented. It keys on what a
//! step CITES: a check-run mail carries its PR's ref for Phase 5's router, and that is not a
//! second delivery of the PR.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_collect_core::no_duplicate_cited_ref;
use bough_plugin_ledger::{Ledger, Order, StepQuery};

/// The ref prefix this row delivers under.
pub const PREFIX: &str = "gh:";

const NAME: &str = "no_duplicate_gh_delivery";

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
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        // The row is being torn down: there is nothing to state about a ledger that is gone.
        return Ok(());
    };
    // Delivery is per agent, and so is duplication: two agents legitimately receive the same PR,
    // and only a second copy on ONE chain is a violation (P6-D15).
    for row in ledger.0.agents().await.map_err(|e| fail(e.to_string()))? {
        let steps = ledger
            .0
            .steps(&StepQuery {
                trajs: vec![row.traj],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .map_err(|e| fail(e.to_string()))?;
        no_duplicate_cited_ref(PREFIX, &steps).map_err(fail)?;
    }
    Ok(())
}
