//! V5's distillation half against the row that ACTUALLY mounts.
//!
//! The other three suites run against `DigestOnly`, a double that always succeeds and always
//! seals. That proves the pass calls the seam; it does not prove the seam's real provider answers.
//! This suite mounts `rollups-summarizer` behind the same handle, so "a pass ADDS a distilled
//! block, seals no tier and edits nothing" is judged against the real code path — including the
//! `rollup/sealed` step, the `rollup/request` row and the digest's own generation.

mod support;

use std::sync::Arc;

use bough_plugin_ledger::{HashScope, RollupKind, RollupQuery, TrajId};
use bough_plugin_rollups::RollupsHandle;
use bough_plugin_rollups_summarizer::{bundle_config, RecapSummarizer, SummarizerInner};
use support::*;

/// The real provider over the same ledger and the same replay adapter the suite already wires.
fn real(m: &Wired) -> RollupsHandle {
    RollupsHandle(Arc::new(RecapSummarizer(Arc::new(SummarizerInner {
        ctx: m.ctx.clone(),
        cfg: Arc::new(bundle_config()),
        ledger: m.ledger.clone(),
        llm: m.llm.clone(),
        composition: "test-composition".into(),
    }))))
}

#[tokio::test]
async fn a_pass_over_the_real_summarizer_adds_a_digest_and_seals_no_tier() {
    // Rounds enough for the judge calls AND the digest call the real provider makes.
    let m = mount_with_rollups(recap_rounds(8), real).await;
    for i in 0..6 {
        evidence(
            &m.ledger,
            "tool/result",
            &[&format!("svc:{i}")],
            serde_json::json!({ "text": format!("a thing that happened, {i}") }),
            t0(),
        )
        .await;
    }
    let before = m
        .ledger
        .0
        .row_hashes(HashScope::All)
        .await
        .expect("hashes read");

    let report = m.recon.run(&request(t0())).await.expect("the pass runs");

    let digest = report
        .distilled
        .expect("the real provider produced a distilled block");
    let rows = m
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![TrajId::new(TRAJ)],
            include_superseded: true,
            ..Default::default()
        })
        .await
        .expect("rollups read");
    assert!(
        rows.iter()
            .any(|r| r.id == digest && r.kind == RollupKind::Digest),
        "the block the report names is a DIGEST row in the store: {rows:?}"
    );
    assert!(
        !rows.iter().any(|r| r.kind == RollupKind::Tier),
        "a reconsolidation pass never seals a tier (§8): {rows:?}"
    );

    // ADDS AND NEVER EDITS: every row that existed before the pass is byte-identical after it.
    let after = m
        .ledger
        .0
        .row_hashes(HashScope::All)
        .await
        .expect("hashes read");
    for row in &before {
        let now = after
            .iter()
            .find(|r| r.table == row.table && r.id == row.id)
            .unwrap_or_else(|| panic!("row `{}` disappeared across a pass", row.id));
        assert_eq!(
            now.hash, row.hash,
            "row `{}` changed across a pass; a pass ADDS and never edits (§8)",
            row.id
        );
    }

    // And the real provider's own bookkeeping landed: the digest was announced and its model call
    // recorded, which the double never had to do.
    assert_eq!(
        steps_of(&m.ledger, "rollup/sealed").await.len(),
        1,
        "one digest, one `rollup/sealed`"
    );
    assert_eq!(
        steps_of(&m.ledger, "rollup/request").await.len(),
        1,
        "the digest's model call is in the ledger (§0.2)"
    );
}

/// The seam REFUSING is a composition choice, not a pass failure: the stub provider makes
/// `/reconsolidate` distil nothing, and everything the pass already appended still stands.
#[tokio::test]
async fn a_refusing_provider_leaves_the_pass_and_its_appends_standing() {
    let m = mount_with_rollups(always_contradiction(8), |m| {
        RollupsHandle(Arc::new(bough_plugin_rollups_none::NoneSummarizer {
            ledger: Arc::new(m.ledger.clone()),
        }))
    })
    .await;
    evidence(
        &m.ledger,
        "tool/result",
        &["svc:x"],
        serde_json::json!({ "text": "one" }),
        t0(),
    )
    .await;
    evidence(
        &m.ledger,
        "tool/result",
        &["svc:x"],
        serde_json::json!({ "text": "two" }),
        t0(),
    )
    .await;

    let report = m
        .recon
        .run(&request(t0()))
        .await
        .expect("a refusing seam is not a failed pass");
    assert!(report.distilled.is_none(), "the stub distils nothing");
    assert_eq!(
        report.contradictions.len(),
        1,
        "the claim the pass already appended stands"
    );
}
