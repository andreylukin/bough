//! §0.2 runtime invariants for `bough-plugin-ledger-sqlite`.
//!
//! The specs are the ledger Definition's — `append_only_rows_never_change`, `seal_once`,
//! `seq_strictly_grows_per_trajectory`, `wake_step_enclosure` — returned from this provider's
//! `Plugin::invariants()` so they run against whichever provider is mounted (P1-D1). The
//! statements live in `bough_plugin_ledger::invariant`; this module only names the plugin.

use bough_kernel::InvariantSpec;

/// The four ledger specs, attributed to this provider.
pub fn specs() -> Vec<InvariantSpec> {
    bough_plugin_ledger::invariant::specs(crate::PLUGIN_NAME)
}
