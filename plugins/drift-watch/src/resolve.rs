//! Invariant (§0.5): validation is PURE and SYNCHRONOUS. A window that cannot hold `min_samples`
//! would flag every agent forever, so it is refused at boot rather than explained at runtime.

use bough_kernel::ConfigError;

use crate::DriftConfig;

/// Refuse: `window_steps == 0`; `min_samples > window_steps`; a `thought_len_cv_flag` that is not
/// finite and positive; a `tool_entropy_flag` outside `0.0..=1.0`.
pub fn validate(_cfg: &DriftConfig) -> Result<(), ConfigError> {
    todo!("WP-4: pure config validation")
}
