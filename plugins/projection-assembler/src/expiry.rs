//! Invariant (§8): the projector HONOURS the appended expiry marker, and honours it under `as_of`
//! exactly as every band does — a marker appended after the request being reproduced did not exist
//! for it (Phase 2 §2.7 item 3). Two bands deliberately do NOT honour expiry: `pins`, because a
//! pin's only relief valve is supersession (§3, V7), and `mail`, because unconsumed mail has its
//! own consumption mechanism and a marker must never silently un-deliver it.

use bough_plugin_projection::{ProjectionError, SectionRequest};
use bough_plugin_rollups::Expired;

/// Load the expiry set for one assembly.
pub async fn load(_req: &SectionRequest) -> Result<Expired, ProjectionError> {
    todo!("WP-5: load the expiry set, honouring as_of")
}
