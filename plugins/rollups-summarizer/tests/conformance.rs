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
    // The suite runs against a trajectory it names and never prepares, so a lenient replay is the
    // honest adapter here: no round is expected, and no round is consumed.
    let fx = fx_with(
        cfg(),
        serde_json::json!([{ "chunks": [{ "type": "end", "stop": "end_turn" }] }]),
    )
    .await;
    let handle = RollupsHandle(Arc::new(fx.summarizer.clone()));
    Conformance { seals: true }
        .run(&handle, &fx.ledger)
        .await
        .expect("the recap provider conforms");
}
