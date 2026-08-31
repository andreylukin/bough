//! §8's relief valve: a suspected-bad tier block is SUPERSEDED — a new block at generation n+1
//! over the same range, plus an APPENDED expiry note — and is never re-summarized in place.

use crate::support;

use bough_plugin_ledger::{StepQuery, StepType};
use bough_plugin_rollups::{Attribution, Summarizer, SupersedeRequest, TierBlock};
use support::*;

#[tokio::test]
async fn a_superseded_block_gets_a_generation_and_an_expiry_note() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    fx.seal().await;
    let before = fx.rollups().await;
    let victim = before.first().expect("a sealed block").clone();
    assert_eq!(victim.superseded_by, None);

    let report = fx
        .summarizer
        .supersede(&SupersedeRequest {
            block: victim.id.clone(),
            reason: "the recap missed the decision".into(),
            at: base() + chrono::Duration::days(2),
            attribution: Attribution::System,
        })
        .await
        .expect("a supersession");

    // Generation n+1, over the SAME range.
    assert_eq!(report.new.as_str(), format!("{}#g1", victim.id));
    let after = fx.rollups().await;
    let new = after
        .iter()
        .find(|r| r.id == report.new)
        .expect("the new block is in the ledger");
    assert_eq!((new.from_seq, new.to_seq), (victim.from_seq, victim.to_seq));
    assert_eq!(new.tier, victim.tier);
    assert_eq!(new.superseded_by, None);

    // The old row is untouched but for the one set-once write.
    let old = after
        .iter()
        .find(|r| r.id == victim.id)
        .expect("the old block is still there — nothing is deleted");
    assert_eq!(old.superseded_by.as_ref(), Some(&report.new));
    assert_eq!(old.body, victim.body, "a sealed body is immutable");
    assert_eq!(old.sealed_at, victim.sealed_at);

    // The block was re-summarized into a NEW row, not edited: a fresh model call produced it.
    let new_body: TierBlock = serde_json::from_value(new.body.clone()).expect("a block body");
    assert_eq!(new_body.tier, victim.tier);
    assert!(
        !new_body.evidence.is_empty(),
        "the new block still indexes raw"
    );

    // And the marker the projector honours is APPENDED, cited, and names the old block.
    let notes = fx
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("memory/expired")],
            ..Default::default()
        })
        .await
        .expect("a read");
    assert_eq!(notes.len(), 1);
    let note = &notes[0];
    assert_eq!(note.id, report.note);
    assert_eq!(note.class, bough_plugin_ledger::Class::Evidence);
    assert!(
        !note.cites.is_empty(),
        "an expiry that cannot say what justified it is not appendable"
    );
    assert_eq!(
        note.body.get("kind").and_then(|v| v.as_str()),
        Some("supersession")
    );
    assert_eq!(
        note.body
            .get("targets")
            .and_then(|v| v.as_array())
            .map(|a| a.len()),
        Some(1)
    );
    assert!(note
        .body
        .get("targets")
        .and_then(|v| v.as_array())
        .map(|a| a[0]
            .as_str()
            .unwrap_or_default()
            .contains(victim.id.as_str()))
        .unwrap_or(false));
    assert_eq!(
        note.body.get("reason").and_then(|v| v.as_str()),
        Some("the recap missed the decision")
    );

    // Seal-once survives it: the range is still sealed exactly once at the live generation.
    let plan = fx
        .summarizer
        .plan(&fx.request(base() + chrono::Duration::days(3)))
        .await
        .expect("a plan");
    assert!(
        !plan.blocks.iter().any(|b| b.from_seq == victim.from_seq),
        "the superseded range was re-planned: {plan:?}"
    );
}
