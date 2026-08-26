//! §3's relief valve, reached from the row that surfaces the suspicion (§8): a suspected-bad tier
//! block is SUPERSEDED — a new block plus an appended expiry note — and never re-summarized in
//! place. Seal-once survives it: the old row's content hash does not move, and the only write it
//! ever accepts is `superseded_by`, set once.

mod common;

use bough_plugin_ledger::{HashScope, Ref, RollupId};
use bough_plugin_rollups::{Attribution, SupersedeRequest};

fn req(block: &RollupId, reason: &str) -> SupersedeRequest {
    SupersedeRequest {
        block: block.clone(),
        reason: reason.to_string(),
        at: common::at(),
        attribution: Attribution::System,
    }
}

#[tokio::test]
async fn a_suspected_bad_block_is_superseded_with_an_expiry_note() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;
    let bad = common::seal_tier(&h, 1, 4).await;

    let report = h
        .drift
        .supersede(&req(&bad.id, "the block says the build passed; it did not"))
        .await
        .expect("a sealed tier this provider owns is supersedable");

    assert_eq!(report.old, bad.id);
    assert_ne!(report.new, bad.id, "supersession MINTS generation n+1");

    // Generation n+1 covers the SAME range at the same tier: it replaces the block, it does not
    // re-cut the window.
    let tiers = common::tiers(&h).await;
    let new = tiers
        .iter()
        .find(|r| r.id == report.new)
        .expect("the new block is in the ledger");
    assert_eq!(
        (new.tier, new.from_seq, new.to_seq),
        (bad.tier, bad.from_seq, bad.to_seq)
    );
    assert_eq!(new.traj, bad.traj);
    assert_eq!(new.superseded_by, None);

    // The appended expiry note names what was expired and why. EVIDENCE, so the ledger refused a
    // note with no cites.
    let note = common::steps_of_kind(&h, common::MEMORY_EXPIRED)
        .await
        .pop()
        .expect("a supersession appends an expiry note");
    assert_eq!(note.id, report.note);
    assert_eq!(note.class, bough_plugin_ledger::Class::Evidence);
    assert_eq!(note.body["kind"], serde_json::json!("supersession"));
    assert!(note.body["reason"]
        .as_str()
        .unwrap()
        .contains("build passed"));
    assert!(note.refs.contains(&Ref::rollup(&bad.id)), "{:?}", note.refs);

    // The old block is still readable and still says what it always said; the default rollup
    // query simply stops returning it.
    let live = h
        .ledger
        .0
        .rollups(&bough_plugin_ledger::RollupQuery {
            trajs: vec![common::traj()],
            kind: Some(bough_plugin_ledger::RollupKind::Tier),
            ..Default::default()
        })
        .await
        .expect("the live tiers read");
    assert_eq!(
        live.iter().map(|r| &r.id).collect::<Vec<_>>(),
        vec![&report.new],
        "a superseded block is excluded from the default query"
    );
}

#[tokio::test]
async fn seal_once_survives_the_supersession() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;
    let bad = common::seal_tier(&h, 1, 4).await;
    let before = common::hashes(&h, HashScope::Rollups).await;
    let before_hash = before
        .iter()
        .find(|(id, _, _)| *id == bad.id.to_string())
        .expect("the block is hashed")
        .1
        .clone();

    let report = h
        .drift
        .supersede(&req(&bad.id, "wrong"))
        .await
        .expect("it supersedes");

    let after = common::hashes(&h, HashScope::Rollups).await;
    let (_, hash, superseded) = after
        .iter()
        .find(|(id, _, _)| *id == bad.id.to_string())
        .expect("the superseded block is STILL in the ledger");
    // The content hash excludes `superseded_by`, so an unchanged hash is exactly the statement
    // that the body, range, prompt_ver and sealed_at were not rewritten.
    assert_eq!(*hash, before_hash, "a sealed block's content was rewritten");
    assert_eq!(superseded.as_deref(), Some(report.new.as_str()));

    // The block itself is byte-identical but for that one column.
    let now = common::tiers(&h)
        .await
        .into_iter()
        .find(|r| r.id == bad.id)
        .expect("the old block is readable");
    assert_eq!(
        (&now.body, &now.prompt_ver, now.sealed_at),
        (&bad.body, &bad.prompt_ver, bad.sealed_at)
    );

    // SET ONCE: a second supersession of the same block is refused, not silently re-pointed.
    let err = h
        .drift
        .supersede(&req(&bad.id, "wrong again"))
        .await
        .expect_err("a block is superseded once");
    assert!(err.to_string().contains("already superseded"), "{err}");
    let still = common::tiers(&h)
        .await
        .into_iter()
        .find(|r| r.id == bad.id)
        .expect("the old block is readable");
    assert_eq!(still.superseded_by, Some(report.new));
}

#[tokio::test]
async fn superseding_a_block_this_provider_did_not_seal_is_refused() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;
    common::seal_tier(&h, 1, 4).await;
    let before = common::hashes(&h, HashScope::Rollups).await;

    // A block from a foreign namespace — the old-feed bridge's interim tier-1 rows are exactly
    // this shape. Supersession is namespaced: a provider mints generation n+1 only over blocks it
    // knows how to have sealed.
    let err = h
        .drift
        .supersede(&req(&RollupId::new("oldfeed:jungler:42"), "suspect"))
        .await
        .expect_err("a foreign block is not this provider's to reseal");
    assert!(
        err.to_string().contains("not a block this provider sealed"),
        "{err}"
    );

    // The refusal wrote NOTHING: no new block, no expiry note, no `superseded_by`.
    assert_eq!(before, common::hashes(&h, HashScope::Rollups).await);
    assert!(common::steps_of_kind(&h, common::MEMORY_EXPIRED)
        .await
        .is_empty());
}
