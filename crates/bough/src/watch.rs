//! Invariant: a rejected candidate never disturbs the running tree. The watch recomposes and calls
//! `kernel.update`; a failed recompose is logged and watching continues, because the kernel has
//! already broadcast `config-update-failed` and the last good tree is still running (§0.3).

use std::sync::Arc;

use bough_kernel::{EffectHandle, Kernel};

use crate::cli::Cli;

/// Watch `bough_util::user_patch_path()` with notify + a debouncer; on change, recompose through
/// `compose_for` and hand the result to `kernel.update`.
///
/// Disposing the returned handle stops watching.
pub fn watch_user_patch(kernel: Arc<Kernel>, cli: Arc<Cli>) -> EffectHandle {
    todo!("WP-5")
}
