//! Invariant (§8): the reset touches the DIGEST and the ABOUT-LINE and nothing else. Sealed tiers
//! are counted before and after and reported; nothing here writes one. The intent half of the
//! fresh about-line starts EMPTY — a reset that carried the old intent forward would be exactly
//! the drift it is meant to undo.

use crate::{DriftError, DriftInner, ResetReport, ResetRequest};

/// Run the reset.
pub async fn run(_inner: &DriftInner, _req: &ResetRequest) -> Result<ResetReport, DriftError> {
    todo!("WP-4: the reset")
}
