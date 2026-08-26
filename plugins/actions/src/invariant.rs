//! §0.2 runtime invariant for `bough-plugin-actions`:
//!
//! **Every journal row has its intent written before its done, and no two rows share an idem
//! key.**
//!
//! Unlike the other invariants in this phase this one reads the `actions` TABLE at quiesce rather
//! than an event stream: the relation it is about is a data relation, and the table is the
//! authority on it. WP-7 owns the check.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::ActionRow;

/// The whole invariant as a pure function of the journal's rows. WP-7.
pub fn evaluate(_rows: &[ActionRow]) -> Result<(), String> {
    todo!("WP-7: intent-before-done, unique idem keys")
}

/// The spec `ActionsPlugin::invariants` returns.
pub fn journal_is_intent_before_done() -> InvariantSpec {
    InvariantSpec {
        name: "action_journal_is_intent_before_done_with_unique_idem_keys",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(_ctx: Context) -> Result<(), InvariantViolation> {
    todo!("WP-7: read the actions table through the ledger handle and evaluate it")
}
