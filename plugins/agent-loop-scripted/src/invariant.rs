//! §0.2 runtime invariant for `bough-plugin-agent-loop-scripted`:
//!
//! **The same request-reconstruction check `agent-loop` holds** — imported, not copied (P2-D18):
//! `bough_plugin_agent_loop::invariant::evaluate_reconstruction` is the one evaluator, and this
//! row is its second recorder. Copying it would let the copies drift, and the whole point of the
//! swap gate is that both providers are held to the SAME ledger protocol.

use std::collections::BTreeSet;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, Order, StepQuery, TrajId};

/// The spec `ScriptedLoopPlugin::invariants` returns.
pub fn requests_reconstruct_from_the_ledger() -> InvariantSpec {
    InvariantSpec {
        name: "every_request_reconstructs_from_the_ledger",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: "every_request_reconstructs_from_the_ledger",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        return Ok(());
    };
    let mut trajs: BTreeSet<TrajId> = BTreeSet::new();
    for row in ledger.0.agents().await.map_err(|e| fail(e.to_string()))? {
        trajs.insert(row.traj);
    }
    let mut steps = Vec::new();
    for traj in trajs {
        steps.extend(
            ledger
                .0
                .steps(&StepQuery {
                    trajs: vec![traj],
                    order: Order::SeqAsc,
                    ..Default::default()
                })
                .await
                .map_err(|e| fail(e.to_string()))?,
        );
    }
    // The recorder is a process-wide static shared with `agent-loop`: partition by fiber, or a
    // second tree in the same process inherits the first's requests and checks them against a
    // store that never held them.
    let mine = ctx.fiber_uid();
    let sent: Vec<_> = bough_plugin_agent_loop::invariant::seen()
        .into_iter()
        .filter(|s| s.fiber == mine)
        .collect();
    // THE shared evaluator. Not a copy of it.
    bough_plugin_agent_loop::invariant::evaluate_reconstruction(&sent, &steps).map_err(fail)
}
