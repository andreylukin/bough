//! §8: a reconsolidation pass only ADDS blocks. Nothing sealed changes, no raw step changes, and
//! nothing is deleted — checked against the ledger's own row hashes rather than against a reading
//! of the code.

mod support;

use bough_plugin_ledger::HashScope;
use support::*;

async fn a_small_history(l: &bough_plugin_ledger::LedgerHandle) {
    for i in 0..4 {
        evidence(
            l,
            "tool/result",
            &[&format!("svc:{i}")],
            serde_json::json!({ "text": format!("fact {i}") }),
            t0() + chrono::Duration::hours(i),
        )
        .await;
    }
}

#[tokio::test]
async fn a_pass_adds_a_distilled_block() {
    let m = mount(always_clear(8)).await;
    a_small_history(&m.ledger).await;

    let report = m
        .recon
        .run(&request(t0() + chrono::Duration::days(1)))
        .await
        .expect("the pass runs");

    let digest = report
        .distilled
        .expect("a pass over a non-empty batch distils");
    let rollups = m
        .ledger
        .0
        .rollups(&bough_plugin_ledger::RollupQuery {
            trajs: vec![bough_plugin_ledger::TrajId::new(TRAJ)],
            ..Default::default()
        })
        .await
        .expect("the query runs");
    assert_eq!(rollups.len(), 1, "exactly one block was added");
    assert_eq!(rollups[0].id, digest);
    assert_eq!(rollups[0].kind, bough_plugin_ledger::RollupKind::Digest);
    // The block came through the SEAM: the `rollup/sealed` step the summarizer appends is there,
    // and reconsolidation never wrote one itself.
    assert_eq!(steps_of(&m.ledger, "rollup/sealed").await.len(), 1);
}

#[tokio::test]
async fn a_pass_changes_no_sealed_row_hash() {
    let m = mount(always_clear(8)).await;
    a_small_history(&m.ledger).await;
    // A first pass seals a digest; the SECOND pass must leave that row exactly as it stands.
    m.recon
        .run(&request(t0() + chrono::Duration::days(1)))
        .await
        .expect("the first pass runs");

    let before = m
        .ledger
        .0
        .row_hashes(HashScope::Rollups)
        .await
        .expect("hashes read");
    m.recon
        .run(&request(t0() + chrono::Duration::days(2)))
        .await
        .expect("the second pass runs");
    let after = m
        .ledger
        .0
        .row_hashes(HashScope::Rollups)
        .await
        .expect("hashes read");

    for b in &before {
        let a = after
            .iter()
            .find(|a| a.id == b.id)
            .unwrap_or_else(|| panic!("sealed row `{}` disappeared", b.id));
        assert_eq!(a.hash, b.hash, "sealed row `{}` was edited", b.id);
    }
}

#[tokio::test]
async fn a_pass_changes_no_raw_step_hash() {
    let m = mount(always_contradiction(8)).await;
    a_small_history(&m.ledger).await;
    // Give the pass something to expire as well, so every one of its three writes is exercised.
    evidence(
        &m.ledger,
        "tool/result",
        &["svc:0"],
        serde_json::json!({ "text": "an old fact" }),
        t0() - chrono::Duration::days(400),
    )
    .await;

    let before = m
        .ledger
        .0
        .row_hashes(HashScope::Steps)
        .await
        .expect("hashes read");
    let report = m
        .recon
        .run(&request(t0() + chrono::Duration::days(1)))
        .await
        .expect("the pass runs");
    assert!(
        !report.expired.is_empty() && !report.contradictions.is_empty(),
        "the pass must actually have written something for this to mean anything"
    );
    let after = m
        .ledger
        .0
        .row_hashes(HashScope::Steps)
        .await
        .expect("hashes read");

    for b in &before {
        let a = after
            .iter()
            .find(|a| a.id == b.id)
            .unwrap_or_else(|| panic!("raw step `{}` disappeared", b.id));
        assert_eq!(a.hash, b.hash, "raw step `{}` was edited", b.id);
    }
    assert!(after.len() > before.len(), "and the pass ADDED rows");

    // The row's own invariant, over the very observations the pass recorded.
    let now = m
        .ledger
        .0
        .row_hashes(HashScope::All)
        .await
        .expect("hashes read");
    // The record is a process-wide static, so this asserts over THIS pass's observation only:
    // another test's pass ran against another ledger and its rows are legitimately absent here.
    let mine: Vec<_> = bough_plugin_reconsolidation::invariant::seen()
        .into_iter()
        .filter(|o| o.pass == report.pass)
        .collect();
    assert_eq!(mine.len(), 1, "the pass recorded itself exactly once");
    bough_plugin_reconsolidation::invariant::evaluate(&mine, &now)
        .expect("a real pass satisfies a_pass_adds_and_never_edits");
}

#[tokio::test]
async fn a_pass_deletes_nothing() {
    let m = mount(always_clear(8)).await;
    a_small_history(&m.ledger).await;
    let before = m
        .ledger
        .0
        .row_hashes(HashScope::All)
        .await
        .expect("hashes read");

    m.recon
        .run(&request(t0() + chrono::Duration::days(1)))
        .await
        .expect("the pass runs");

    let after = m
        .ledger
        .0
        .row_hashes(HashScope::All)
        .await
        .expect("hashes read");
    for b in &before {
        assert!(
            after.iter().any(|a| a.table == b.table && a.id == b.id),
            "row `{}` of `{}` was deleted",
            b.id,
            b.table
        );
    }
}
