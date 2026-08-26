//! §8: stale evidence is expired by an APPENDED marker, never by an edit. The marker is EVIDENCE,
//! so the ledger itself refuses one that cannot say what justified it; and a pin is never a
//! candidate, whatever the config says (V7).

mod support;

use bough_plugin_ledger::Class;
use bough_plugin_reconsolidation::vocabulary::MemoryExpired;
use bough_plugin_reconsolidation::{ReconKind, MEMORY_EXPIRED};
use support::*;

/// One step old enough to be stale, and one fresh one.
async fn an_old_and_a_new_fact(
    l: &bough_plugin_ledger::LedgerHandle,
) -> (bough_plugin_ledger::Step, bough_plugin_ledger::Step) {
    let old = evidence(
        l,
        "tool/result",
        &["svc:old"],
        serde_json::json!({ "text": "an old fact" }),
        t0() - chrono::Duration::days(400),
    )
    .await;
    let new = evidence(
        l,
        "tool/result",
        &["svc:new"],
        serde_json::json!({ "text": "a fresh fact" }),
        t0(),
    )
    .await;
    (old, new)
}

#[tokio::test]
async fn stale_evidence_is_expired_by_an_appended_marker() {
    let m = mount(always_clear(8)).await;
    let (old, new) = an_old_and_a_new_fact(&m.ledger).await;

    let report = m.recon.run(&request(t0())).await.expect("the pass runs");

    assert_eq!(report.expired.len(), 1, "only the old fact is stale");
    let markers = steps_of(&m.ledger, MEMORY_EXPIRED).await;
    assert_eq!(markers.len(), 1);
    let body: MemoryExpired =
        serde_json::from_value((*markers[0].body).clone()).expect("a memory/expired body");
    assert_eq!(body.kind, ReconKind::Expiry);
    assert_eq!(
        body.targets,
        vec![bough_plugin_ledger::Ref::new(format!("step:{}", old.id))]
    );
    // The step it names is UNTOUCHED: expiry is a marker the projector honours, not an edit.
    let still_there = m
        .ledger
        .0
        .step(&old.id)
        .await
        .expect("read")
        .expect("still there");
    assert_eq!(still_there.body, old.body);
    // And the fresh fact was not named.
    assert!(!format!("{body:?}").contains(new.id.as_str()));
}

#[tokio::test]
async fn the_marker_cites_what_justified_it() {
    let m = mount(always_clear(8)).await;
    let (old, _) = an_old_and_a_new_fact(&m.ledger).await;

    m.recon.run(&request(t0())).await.expect("the pass runs");

    let markers = steps_of(&m.ledger, MEMORY_EXPIRED).await;
    assert_eq!(
        markers[0].class,
        Class::Evidence,
        "the marker is evidence, so the ledger refuses one with no cites"
    );
    let cited: Vec<String> = markers[0]
        .cites
        .iter()
        .map(|c| c.r#ref.to_string())
        .collect();
    assert_eq!(cited, vec![format!("step:{}", old.id)]);
    let body: MemoryExpired = serde_json::from_value((*markers[0].body).clone()).expect("a body");
    assert!(
        body.reason.contains("400") && body.reason.contains("90"),
        "the reason must say how old it is and against which threshold: {}",
        body.reason
    );
}

#[tokio::test]
async fn a_pin_is_never_an_expiry_candidate() {
    let m = mount(always_clear(8)).await;
    // A pin far older than the threshold. §3: a pin rides every projection regardless of age, and
    // its only relief valve is supersession.
    let pin = m
        .ledger
        .0
        .append(bough_plugin_ledger::Append {
            traj: bough_plugin_ledger::TrajId::new(TRAJ),
            wake: bough_plugin_ledger::WakeId::new("w1"),
            kind: bough_plugin_ledger::StepType::new("pin/set"),
            class: Class::Thought,
            body: serde_json::json!({ "title": "the rule", "text": "never force-push main" }),
            cites: vec![],
            at: t0() - chrono::Duration::days(4000),
            id: None,
        })
        .await
        .expect("the pin appends");

    let plan = m.recon.plan(&request(t0())).await.expect("the plan builds");
    assert!(
        plan.expiry_candidates.is_empty(),
        "a pin is not a candidate: {:?}",
        plan.expiry_candidates
    );
    let report = m.recon.run(&request(t0())).await.expect("the pass runs");
    assert!(report.expired.is_empty());
    assert!(steps_of(&m.ledger, MEMORY_EXPIRED).await.is_empty());
    // And the pin itself is untouched.
    assert!(m.ledger.0.step(&pin.id).await.expect("read").is_some());
}

#[tokio::test]
async fn expiring_the_same_step_twice_appends_one_more_marker_and_changes_nothing_else() {
    let m = mount(always_clear(8)).await;
    an_old_and_a_new_fact(&m.ledger).await;

    m.recon
        .run(&request(t0()))
        .await
        .expect("the first pass runs");
    let before = m
        .ledger
        .0
        .row_hashes(bough_plugin_ledger::HashScope::Steps)
        .await
        .expect("hashes read");
    let markers_before = steps_of(&m.ledger, MEMORY_EXPIRED).await.len();

    m.recon
        .run(&request(t0()))
        .await
        .expect("the second pass runs");

    let markers_after = steps_of(&m.ledger, MEMORY_EXPIRED).await;
    assert_eq!(
        markers_after.len(),
        markers_before + 1,
        "a second expiry APPENDS a second marker rather than editing the first"
    );
    let after = m
        .ledger
        .0
        .row_hashes(bough_plugin_ledger::HashScope::Steps)
        .await
        .expect("hashes read");
    for b in &before {
        let a = after
            .iter()
            .find(|a| a.id == b.id)
            .unwrap_or_else(|| panic!("step row `{}` disappeared", b.id));
        assert_eq!(a.hash, b.hash, "step row `{}` changed", b.id);
    }
    assert_eq!(
        after.len(),
        before.len() + 2,
        "and exactly two rows were added: the marker and the second `rollup/sealed`"
    );
}
