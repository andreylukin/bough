//! §8: stale evidence is expired by an APPENDED marker, never by an edit. The marker is EVIDENCE,
//! so the ledger itself refuses one that cannot say what justified it; and a pin is never a
//! candidate, whatever the config says (V7).

use crate::support;

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

/// V7, the PASS side. `detect::stale` refuses a pin by kind and `resolve::validate` refuses one at
/// boot, but the CONTRADICTION path reaches the expiry writer without going through either: a
/// confirmed pair pushes its older half straight into the candidate list. `pin/set` is
/// `ClassRule::Either`, so a pin can legally be EVIDENCE and `detect::pairs` will pair it.
///
/// The claim still stands — surfacing a disagreement about a pin is exactly what a pass is for —
/// but no `memory/expired` marker may ever name it. A pin's only relief valve is supersession.
#[tokio::test]
async fn a_confirmed_contradiction_never_expires_a_pin() {
    let m = mount(always_contradiction(8)).await;
    // A PIN and a newer ordinary fact, sharing a ref so they pair.
    let pin = m
        .ledger
        .0
        .append(bough_plugin_ledger::Append {
            traj: bough_plugin_ledger::TrajId::new(TRAJ),
            wake: bough_plugin_ledger::WakeId::new("w1"),
            kind: bough_plugin_ledger::StepType::new("pin/set"),
            class: Class::Evidence,
            body: serde_json::json!({
                "title": "the port",
                "text": "the service listens on 8080",
                "supersedes": []
            }),
            cites: vec![bough_plugin_ledger::Cite {
                r#ref: bough_plugin_ledger::Ref::new("svc:port"),
                url: None,
            }],
            at: t0() - chrono::Duration::days(10),
            id: None,
        })
        .await
        .expect("the pin appends");
    let newer = evidence(
        &m.ledger,
        "tool/result",
        &["svc:port"],
        serde_json::json!({ "text": "the service listens on 9090" }),
        t0(),
    )
    .await;

    let report = m.recon.run(&request(t0())).await.expect("the pass runs");

    assert_eq!(
        report.contradictions.len(),
        1,
        "the disagreement is still SURFACED as a claim"
    );
    let markers = steps_of(&m.ledger, MEMORY_EXPIRED).await;
    for marker in &markers {
        let body: MemoryExpired =
            serde_json::from_value((*marker.body).clone()).expect("a memory/expired body");
        assert!(
            !body
                .targets
                .iter()
                .any(|t| t.as_str() == format!("step:{}", pin.id)),
            "a marker named the pin `{}`: a pin is NEVER expired (§3, V7): {body:?}",
            pin.id
        );
    }
    // The pin's row is untouched, and the newer half was never a candidate either (it stands).
    assert_eq!(
        m.ledger
            .0
            .step(&pin.id)
            .await
            .expect("read")
            .expect("still there")
            .body,
        pin.body
    );
    assert!(!report.expired.contains(&newer.id));
}

/// §0.2, model-visible ⟺ ledgered: every judge call leaves a `recon/request` step naming the model
/// and the tokens, under the RUNNING pass's wake — not a fabricated constant one.
#[tokio::test]
async fn every_judge_call_is_recorded_as_a_step_under_the_pass_wake() {
    let m = mount(always_clear(8)).await;
    let a = evidence(
        &m.ledger,
        "tool/result",
        &["svc:x"],
        serde_json::json!({ "text": "one" }),
        t0(),
    )
    .await;
    let b = evidence(
        &m.ledger,
        "tool/result",
        &["svc:x"],
        serde_json::json!({ "text": "two" }),
        t0(),
    )
    .await;

    let report = m.recon.run(&request(t0())).await.expect("the pass runs");
    assert!(report.calls >= 1);

    let requests = steps_of(&m.ledger, bough_plugin_reconsolidation::RECON_REQUEST).await;
    assert_eq!(
        requests.len(),
        1,
        "one pair judged, one request row: {requests:?}"
    );
    let body: bough_plugin_reconsolidation::ReconRequest =
        serde_json::from_value((*requests[0].body).clone()).expect("a recon/request body");
    assert_eq!(
        body.prompt_ver,
        bough_plugin_reconsolidation::prompts::RECON_1
    );
    assert!(
        !body.model.is_empty(),
        "the model that answered is recorded"
    );
    assert!(!body.failed);
    assert_eq!(body.older, a.id.to_string());
    assert_eq!(body.newer, b.id.to_string());
    assert!(!body.input_digest.is_empty());
    // The wake is the RUNNING pass's, so the row is greppable back to the pass that wrote it.
    assert_eq!(
        requests[0].wake.to_string(),
        format!("recon:{}", report.pass),
        "the request rides the pass's own wake (P4-D2)"
    );
    assert_ne!(requests[0].wake.to_string(), "recon:plan");
}
