//! §0.2 runtime invariants for `bough-plugin-ledger-memory`.
//!
//! The specs are the ledger Definition's — `append_only_rows_never_change`, `seal_once`,
//! `seq_strictly_grows_per_trajectory`, `wake_step_enclosure` — returned from this provider's
//! `Plugin::invariants()` so the same four checks run whichever provider is mounted (P1-D1).

use bough_kernel::InvariantSpec;

/// The four ledger specs, attributed to this provider.
pub fn specs() -> Vec<InvariantSpec> {
    bough_plugin_ledger::invariant::specs(crate::PLUGIN_NAME)
}
