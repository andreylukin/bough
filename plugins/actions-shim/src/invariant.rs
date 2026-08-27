//! §0.2 runtime invariant for `bough-plugin-actions-shim`:
//!
//! **One `gh` invocation per `action/intent` idem key, over the process's whole life.** This is
//! §7's "never re-executed" fact, checked continuously rather than only by V3's crash test: an
//! idem key that was acted on twice is the exact failure a journal + marker exists to prevent.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::IdemKey;

/// The invariant's name, as a violation reports it.
pub const NAME: &str = "one_gh_invocation_per_idem_key";

/// Record one invocation. Called from `execute`, immediately before the outward act.
///
/// WP-4.
pub fn record(idem: &IdemKey) {
    let _ = idem;
    todo!("WP-4: bump this idem key's invocation count")
}

/// Every idem key this process invoked, with its count.
///
/// WP-4.
pub fn invocations() -> Vec<(IdemKey, u32)> {
    todo!("WP-4: the recorded counts")
}

/// Forget the record. The row's disposal path.
///
/// WP-4.
pub fn forget() {
    todo!("WP-4: clear the recorded counts")
}

/// PURE: the check — no idem key was invoked twice.
///
/// WP-4.
pub fn check_counts(counts: &[(IdemKey, u32)]) -> Result<(), String> {
    let _ = counts;
    todo!("WP-4: any count > 1 is a violation naming the key")
}

/// The specs this crate contributes.
///
/// WP-4: return the spec once [`check_counts`] and the recorder land.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}

#[allow(dead_code)]
async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let _ = (ctx, Cadence::OnQuiesce, NAME);
    todo!("WP-4: read the recorded counts and call check_counts")
}
