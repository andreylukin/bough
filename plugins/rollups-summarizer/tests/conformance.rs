//! The rollups seam's provider-conformance suite, run against THIS provider with `seals: true`.
//! `rollups-none` runs the same suite with `seals: false`, so both providers are judged by one
//! statement of the contract rather than by two specs written twice.

mod support;

use bough_plugin_rollups::conformance::Conformance;
use bough_plugin_rollups::RollupsHandle;
use std::sync::Arc;
use support::*;

#[tokio::test]
async fn the_recap_provider_passes_the_rollups_conformance_suite() {
    // The suite's second half PREPARES a history and expects this provider to seal over it, so
    // the fixture supplies real recap answers — one per call the pass and the digest rebuild make.
    let fx = fx(cfg(), 32).await;
    let handle = RollupsHandle(Arc::new(fx.summarizer.clone()));
    Conformance { seals: true }
        .run(&handle, &fx.ledger)
        .await
        .expect("the recap provider conforms");
}
