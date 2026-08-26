//! Invariant (§8): a digest rebuild READS sealed tiers and writes none. It supersedes the previous
//! digest and repoints `agents.digest_rollup`; the tier count on the trajectory is unchanged
//! across it, which is what `/reset` relies on.

use bough_plugin_rollups::{DigestReport, DigestRequest, RollupsError};

use crate::SummarizerInner;

/// Rebuild the standing digest. `from_raw` ignores the existing digest entirely.
pub async fn rebuild(
    _inner: &SummarizerInner,
    _req: &DigestRequest,
) -> Result<DigestReport, RollupsError> {
    todo!("WP-2: digest rebuild")
}
