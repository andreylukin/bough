//! Invariant (§0.5): validation is PURE and SYNCHRONOUS, and misconfiguration fails loud. A
//! config listing a kind in [`bough_plugin_rollups::NEVER_EXPIRABLE`] is refused at boot rather
//! than silently intersected away at runtime — the runtime intersection stays as the second lock.

use bough_kernel::ConfigError;

use crate::ReconConfig;

/// Refuse: `batch_steps == 0`; `max_calls_per_pass == 0`; `stale_after_days <= 0`; an
/// `expirable_kinds` entry that is never expirable (V7).
pub fn validate(_cfg: &ReconConfig) -> Result<(), ConfigError> {
    todo!("WP-3: pure config validation")
}
