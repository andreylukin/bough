//! V2 — the tier tree: fanout and coverage as CONFIGURED, every block an index into the layer
//! beneath it, every ref resolvable, and nothing sealed inside the verbatim tail's ground (§3, §8).
//!
//! Offline: `ledger-memory` + `llm-replay`.

mod support;

use bough_plugin_ledger::{RollupKind, Seq};
use bough_plugin_rollups::{block, Beneath, TierBlock};
use support::*;

fn tier_blocks(fx: &[bough_plugin_ledger::Rollup], tier: u8) -> Vec<&bough_plugin_ledger::Rollup> {
    fx.iter()
        .filter(|r| r.kind == RollupKind::Tier && r.tier == tier)
        .collect()
}

fn body(r: &bough_plugin_ledger::Rollup) -> TierBlock {
    serde_json::from_value(r.body.clone()).expect("this crate's own block body parses")
}

/// §3's arithmetic, on real rows: `max_window_steps: 10` and an episode per wake means a tier-1
/// block covers about ten steps.
#[tokio::test]
async fn tier_one_blocks_cover_about_ten_steps() {
    let fx = fx(cfg(), 16).await;
    fx.seed(4, 10).await;
    let report = fx.seal().await;
    assert!(!report.sealed.is_empty(), "{report:?}");
    let rollups = fx.rollups().await;
    let ones = tier_blocks(&rollups, 1);
    assert_eq!(
        ones.len(),
        3,
        "one block per CLOSED episode; the fourth is still the open head"
    );
    for r in &ones {
        let covered = r.to_seq.0 - r.from_seq.0 + 1;
        assert_eq!(
            covered, 10,
            "tier 1 covers max_window_steps steps, got {covered}"
        );
    }
}

/// V2's fanout half: ten tier-1 blocks and exactly one tier-2 block over them.
#[tokio::test]
async fn ten_tier_one_blocks_reduce_to_one_tier_two_block() {
    let fx = fx(cfg(), 32).await;
    fx.seed(11, 10).await;
    let report = fx.seal().await;
    let rollups = fx.rollups().await;
    let ones = tier_blocks(&rollups, 1);
    let twos = tier_blocks(&rollups, 2);
    assert_eq!(ones.len(), 10, "the 11th episode is still the open head");
    assert_eq!(twos.len(), 1, "fanout 10 reduces exactly ten children");
    let two = body(twos[0]);
    match &two.beneath {
        Beneath::Blocks { rollups } => assert_eq!(rollups.len(), 10),
        other => panic!("a tier-2 block reduces blocks, not {other:?}"),
    }
    assert_eq!(twos[0].from_seq, ones[0].from_seq);
    assert_eq!(twos[0].to_seq, ones[9].to_seq);
    assert_eq!(report.stop, bough_plugin_rollups::Stop::Complete);
}

/// §3: "every block carries refs into the raw beneath it".
#[tokio::test]
async fn every_sealed_block_names_refs_into_the_layer_beneath_it() {
    let fx = fx(cfg(), 32).await;
    fx.seed(11, 10).await;
    fx.seal().await;
    let rollups = fx.rollups().await;
    for r in rollups.iter().filter(|r| r.kind == RollupKind::Tier) {
        let b = body(r);
        let (steps, blocks) = block::refs_of(&b);
        match (&b.beneath, r.tier) {
            (Beneath::Raw { steps: beneath }, 1) => {
                assert!(!beneath.is_empty(), "a tier-1 block names raw steps");
                assert!(blocks.is_empty());
            }
            (Beneath::Blocks { rollups: beneath }, t) if t > 1 => {
                assert_eq!(beneath.len(), 10);
                // P4-D5: a COARSE block still resolves to raw in one hop.
                assert!(
                    !b.evidence.is_empty(),
                    "a tier-{t} block carries raw evidence of its own"
                );
            }
            (other, t) => panic!("tier {t} has the wrong layer beneath: {other:?}"),
        }
        assert!(!steps.is_empty(), "every block names raw steps somewhere");
    }
}

/// Tiers are an INDEX, never a replacement: every id a block names is a row that exists.
#[tokio::test]
async fn every_ref_in_a_sealed_block_resolves_to_an_existing_step_or_rollup() {
    let fx = fx(cfg(), 32).await;
    fx.seed(11, 10).await;
    fx.seal().await;
    let rollups = fx.rollups().await;
    for r in rollups.iter().filter(|r| r.kind == RollupKind::Tier) {
        let (steps, blocks) = block::refs_of(&body(r));
        for id in steps {
            assert!(
                fx.ledger.0.step(&id).await.expect("a read").is_some(),
                "`{}` names step `{id}`, which is not in the ledger",
                r.id
            );
        }
        for id in blocks {
            assert!(
                rollups.iter().any(|x| x.id == id),
                "`{}` names rollup `{id}`, which is not in the ledger",
                r.id
            );
        }
    }
}

/// P1-D13: a real set when the covered steps carry refs, computed from the STEPS and not from the
/// model's answer.
#[tokio::test]
async fn notable_refs_are_the_covered_steps_domain_refs() {
    let fx = fx(cfg(), 16).await;
    fx.seed(4, 10).await;
    fx.seal().await;
    let steps = fx.steps().await;
    let rollups = fx.rollups().await;
    let ones = tier_blocks(&rollups, 1);
    assert!(!ones.is_empty());
    for r in ones {
        let covered: Vec<_> = steps
            .iter()
            .filter(|s| s.seq >= r.from_seq && s.seq <= r.to_seq)
            .cloned()
            .collect();
        assert_eq!(
            r.notable_refs,
            block::notable_refs(&covered, cfg().max_notable_refs),
            "`{}` is notable for something its steps do not name",
            r.id
        );
        assert!(
            !r.notable_refs.is_empty(),
            "these episodes cite a github ref, so the set is not the everyone-set"
        );
    }
}

#[tokio::test]
async fn blocks_are_stamped_with_prompt_ver_and_sealed_at() {
    let fx = fx(cfg(), 16).await;
    fx.seed(4, 10).await;
    let at = fx.request(base() + chrono::Duration::days(1)).at;
    fx.seal().await;
    for r in fx.rollups().await {
        assert_eq!(r.prompt_ver, cfg().prompt_ver, "`{}` is unstamped", r.id);
        assert_eq!(
            r.sealed_at, at,
            "the clock is the caller's, not the store's"
        );
    }
}

/// P4-D11: a sealed tier and the verbatim tail never describe the same steps.
#[tokio::test]
async fn the_pass_never_seals_within_seal_lag_steps_of_the_head() {
    let mut c = cfg();
    c.seal_lag_steps = 25;
    let fx = fx(c.clone(), 16).await;
    fx.seed(4, 10).await;
    fx.seal().await;
    let head = fx.head().await;
    let ceiling = Seq(head.0 - c.seal_lag_steps as u64);
    let rollups = fx.rollups().await;
    assert!(!rollups.is_empty(), "something was sealed below the lag");
    for r in &rollups {
        assert!(
            r.to_seq <= ceiling,
            "`{}` seals up to {} but the head is {} and the lag is {}",
            r.id,
            r.to_seq.0,
            head.0,
            c.seal_lag_steps
        );
    }
}
