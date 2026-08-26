//! Invariant (§0.5): validation is PURE and SYNCHRONOUS, and a stamp that names a prompt the
//! binary does not have is a lie — so `prompt_ver` must resolve in [`crate::prompts`] or the row
//! refuses to boot. Misconfiguration fails loud.

use bough_kernel::ConfigError;
use bough_plugin_rollups::{TierCfg, WindowCfg};

use crate::SummarizerConfig;

/// Refuse: an empty or unknown `prompt_ver`; `fanout < 2`; `max_tier == 0`;
/// `min_window_steps > max_window_steps`; `max_calls_per_pass == 0`; `gap_minutes == 0`.
pub fn validate(_cfg: &SummarizerConfig) -> Result<(), ConfigError> {
    todo!("WP-2: pure config validation")
}

/// The tier shape this row builds, derived from the validated config.
pub fn tier_cfg(_cfg: &SummarizerConfig) -> TierCfg {
    todo!("WP-2: TierCfg from the row config")
}

/// The episode cut this row uses, derived from the validated config.
pub fn window_cfg(_cfg: &SummarizerConfig) -> WindowCfg {
    todo!("WP-2: WindowCfg from the row config")
}
