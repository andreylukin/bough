//! §8: contradiction detection SURFACES claim steps. A pass proposes; it never resolves — the
//! accept/reject surface is Phase 5's — and a pair the model clears leaves no trace at all.

mod support;

use bough_plugin_ledger::vocabulary::ClaimProposed;
use support::*;

/// Two evidence steps about the same ref, disagreeing about the port.
async fn a_planted_pair(
    l: &bough_plugin_ledger::LedgerHandle,
) -> (bough_plugin_ledger::Step, bough_plugin_ledger::Step) {
    let older = evidence(
        l,
        "tool/result",
        &["svc:api"],
        serde_json::json!({ "text": "the api listens on 8080" }),
        t0(),
    )
    .await;
    let newer = evidence(
        l,
        "tool/result",
        &["svc:api"],
        serde_json::json!({ "text": "the api listens on 9090" }),
        t0() + chrono::Duration::hours(1),
    )
    .await;
    (older, newer)
}

#[tokio::test]
async fn a_planted_contradiction_is_recorded_as_a_claim_step() {
    let m = mount(always_contradiction(4)).await;
    a_planted_pair(&m.ledger).await;

    let report = m
        .recon
        .run(&request(t0() + chrono::Duration::hours(2)))
        .await
        .expect("the pass runs");

    assert_eq!(report.contradictions.len(), 1, "one pair, one claim");
    let claims = steps_of(&m.ledger, "claim/proposed").await;
    assert_eq!(claims.len(), 1);
    let body: ClaimProposed =
        serde_json::from_value((*claims[0].body).clone()).expect("a claim/proposed body");
    assert_eq!(body.kind, "contradiction");
    assert!(
        body.body.contains("disagree"),
        "the verdict is the body: {body:?}"
    );
    // A proposal, never a resolution: nothing accepted or rejected it.
    assert!(steps_of(&m.ledger, "claim/accepted").await.is_empty());
    assert!(steps_of(&m.ledger, "claim/rejected").await.is_empty());
}

#[tokio::test]
async fn the_claim_cites_both_conflicting_steps() {
    let m = mount(always_contradiction(4)).await;
    let (older, newer) = a_planted_pair(&m.ledger).await;

    m.recon
        .run(&request(t0() + chrono::Duration::hours(2)))
        .await
        .expect("the pass runs");

    let claims = steps_of(&m.ledger, "claim/proposed").await;
    let cited: Vec<String> = claims[0]
        .cites
        .iter()
        .map(|c| c.r#ref.to_string())
        .collect();
    assert!(
        cited.contains(&format!("step:{}", older.id))
            && cited.contains(&format!("step:{}", newer.id)),
        "the claim must cite BOTH steps, got {cited:?}"
    );
    // And the cites are in the canonical ref index, so a projection can route on either step.
    assert!(claims[0]
        .refs
        .contains(&bough_plugin_ledger::Ref::new(format!("step:{}", older.id))));
}

#[tokio::test]
async fn a_pair_the_model_clears_produces_no_claim() {
    let m = mount(always_clear(4)).await;
    a_planted_pair(&m.ledger).await;

    let report = m
        .recon
        .run(&request(t0() + chrono::Duration::hours(2)))
        .await
        .expect("the pass runs");

    assert!(
        report.contradictions.is_empty(),
        "a cleared pair is not a claim"
    );
    assert!(steps_of(&m.ledger, "claim/proposed").await.is_empty());
    assert!(report.calls > 0, "the model WAS asked; it simply said no");
}
