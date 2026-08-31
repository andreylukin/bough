//! §0.2 runtime invariant for `bough-plugin-collector-slack`:
//!
//! **No two `mail/delivered` steps on one trajectory CITE the same `slack:` ref.** The
//! at-least-once ref guard, checked against the ledger rather than documented.
//!
//! There is no key half (compare `collector-linear`): this row holds NO credential at all. The
//! Slack token lives on the `mcp.rmcp` server row as a `${keychain:…}` reference and never enters
//! this process's config, so there is nothing to scan the ledger for.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_collect_core::no_duplicate_cited_ref;
use bough_plugin_ledger::{Ledger, Order, StepQuery};

/// The ref prefix this row delivers under.
pub const PREFIX: &str = "slack:";

const NAME: &str = "no_duplicate_slack_delivery";

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
        return Ok(());
    };
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
