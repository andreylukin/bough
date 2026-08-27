//! §0.2 runtime invariant for `bough-plugin-tui-preview`:
//!
//! **Every rendered preview whose `as_of` names a `request/header` in the ledger carries that
//! header's `projection_digest`.** The pane cannot render bytes the ledger does not describe:
//! if the two ever disagree, the pane is showing something no wake ever sent, which is exactly
//! the lie a "byte-exact preview" must never tell (§16).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};

/// The invariant's name, as a violation reports it.
pub const NAME: &str = "every_render_matches_its_headers_projection_digest";

/// What the last frame put on screen: the `as_of` it assembled at and the digest of the bytes it
/// painted. Recorded from `Pane::render`; allocation-only, no I/O.
///
/// WP-1.
pub fn record(as_of: bough_plugin_ledger::Seq, digest: &str) {
    let _ = (as_of, digest);
    todo!("WP-1: record the rendered (as_of, digest) pair")
}

/// The recorded frame, if there is one.
///
/// WP-1.
pub fn rendered() -> Option<(bough_plugin_ledger::Seq, String)> {
    todo!("WP-1: the last recorded (as_of, digest) pair")
}

/// Forget the recorded frame. The row's disposal path: a disabled row leaves nothing behind.
///
/// WP-1.
pub fn forget() {
    todo!("WP-1: clear the recorded frame")
}

/// PURE: the check, over the rendered pair and the digest the matching `request/header` carries.
/// `None` for the header means no wake assembled at that `as_of`, which is not a violation.
///
/// WP-1.
pub fn check_render(rendered: Option<(&str, Option<&str>)>) -> Result<(), String> {
    let _ = rendered;
    todo!("WP-1: rendered digest must equal the header's projection_digest when there is one")
}

/// The specs this crate contributes.
///
/// WP-1: return the spec once [`check_render`] and the recorder land.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}

#[allow(dead_code)]
async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let _ = (ctx, Cadence::OnQuiesce, NAME);
    todo!("WP-1: read the recorded frame, look its header up, call check_render")
}
