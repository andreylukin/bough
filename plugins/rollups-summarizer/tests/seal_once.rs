//! V1 — seal-once (§3, §8): a raw segment is summarized EXACTLY ONCE, a sealed row is immutable
//! afterwards, and `superseded_by` is the one set-once write.
//!
//! Offline: `ledger-memory` + `llm-replay`. The one live case is `#[ignore]`d.

use crate::support;

use std::collections::BTreeMap;

use bough_plugin_ledger::{HashScope, RollupId, StepQuery, StepType};
use bough_plugin_rollups::{Attribution, RollupsError, Stop, Summarizer, SupersedeRequest};
use bough_plugin_rollups_summarizer::SummarizerConfig;
use support::*;

/// `rollup/sealed` bodies, keyed by the rollup they name.
async fn sealed_steps(fx: &Fx) -> BTreeMap<String, usize> {
    let steps = fx
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("rollup/sealed")],
            ..Default::default()
        })
        .await
        .expect("a read");
    let mut out: BTreeMap<String, usize> = BTreeMap::new();
    for s in steps {
        let id = s
            .body
            .get("rollup")
            .and_then(|v| v.as_str())
            .expect("a rollup/sealed names its rollup")
            .to_string();
        *out.entry(id).or_default() += 1;
    }
    out
}

#[tokio::test]
async fn a_range_is_summarised_exactly_once() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    let first = fx.seal().await;
    assert_eq!(first.sealed.len(), 3);

    // A second pass over the same ledger. Nothing new is sealed and nothing is re-announced.
    let second = fx.seal().await;
    assert!(second.sealed.is_empty(), "{second:?}");
    let announced = sealed_steps(&fx).await;
    assert_eq!(announced.len(), 3);
    for (id, n) in &announced {
        assert_eq!(*n, 1, "`{id}` was announced {n} times");
    }
    // And the ranges are disjoint: no two blocks describe the same step.
    let mut ranges: Vec<(u64, u64)> = fx
        .rollups()
        .await
        .iter()
        .map(|r| (r.from_seq.0, r.to_seq.0))
        .collect();
    ranges.sort();
    for pair in ranges.windows(2) {
        assert!(pair[0].1 < pair[1].0, "overlapping ranges: {pair:?}");
    }
}

/// The planner refuses BEFORE the store does — and the store refuses too, which is the belt to
/// the planner's braces.
#[tokio::test]
async fn re_sealing_the_same_range_is_refused() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    fx.seal().await;

    let plan = fx
        .summarizer
        .plan(&fx.request(base() + chrono::Duration::days(1)))
        .await
        .expect("a plan");
    assert!(plan.blocks.is_empty(), "nothing is left to plan");
    let already = plan
        .skipped
        .iter()
        .filter(|s| s.why == bough_plugin_rollups::SkipReason::AlreadySealed)
        .count();
    assert_eq!(already, 3, "each sealed range is skipped BY NAME: {plan:?}");

    // The BELT to those braces: even a stale plan is refused at the write, with the error §3's
    // seal-once is written in.
    let victim = fx.rollups().await.first().expect("a sealed block").clone();
    let stale = bough_plugin_rollups::PlannedBlock {
        id: victim.id.clone(),
        tier: victim.tier,
        from_seq: victim.from_seq,
        to_seq: victim.to_seq,
        inputs: bough_plugin_rollups::Inputs::Raw(vec![]),
        windows: vec![],
    };
    let err =
        bough_plugin_rollups_summarizer::seal::refuse_if_sealed(&fx.summarizer.0, &traj(), &stale)
            .await
            .expect_err("a sealed range is refused at the write too");
    match err {
        RollupsError::AlreadySealed {
            existing, from, to, ..
        } => {
            assert_eq!(existing, victim.id);
            assert_eq!((from, to), (victim.from_seq, victim.to_seq));
        }
        other => panic!("wrong refusal: {other}"),
    }
}

#[tokio::test]
async fn a_second_pass_over_an_unchanged_ledger_seals_nothing() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    fx.seal().await;
    let before = fx.rollups().await.len();
    let steps_before = fx.steps().await.len();

    let second = fx.seal().await;
    assert_eq!(second.stop, Stop::NothingToDo);
    assert_eq!(second.calls, 0, "a no-op pass calls no model");
    assert_eq!(second.tokens_in, 0);
    assert_eq!(fx.rollups().await.len(), before);
    assert_eq!(
        fx.steps().await.len(),
        steps_before,
        "a no-op pass appends nothing, not even a request step"
    );
}

/// P4-D4: the block id excludes `prompt_ver`, so improving the recap prompt does NOT retroactively
/// re-summarize history. Supersession does, one block at a time, on purpose.
#[tokio::test]
async fn a_prompt_ver_bump_does_not_re_open_a_sealed_range() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    fx.seal().await;
    let before = fx.rollups().await;
    assert!(before.iter().all(|r| r.prompt_ver == "r4.1"));

    let bumped = fx.reconfigured(SummarizerConfig {
        prompt_ver: bough_plugin_rollups_summarizer::prompts::R4_2.to_string(),
        ..cfg()
    });
    let plan = bumped
        .plan(&fx.request(base() + chrono::Duration::days(2)))
        .await
        .expect("a plan");
    assert!(
        plan.blocks.is_empty(),
        "a prompt bump re-opened {} range(s)",
        plan.blocks.len()
    );
    let report = bumped
        .seal(&fx.request(base() + chrono::Duration::days(2)))
        .await
        .expect("a pass");
    assert_eq!(report.stop, Stop::NothingToDo);
    let after = fx.rollups().await;
    assert_eq!(after.len(), before.len());
    assert!(
        after.iter().all(|r| r.prompt_ver == "r4.1"),
        "a sealed block keeps the stamp that produced it"
    );
}

/// §3: the ONLY write a sealed row accepts is `superseded_by`, and `row_hashes` excludes it by
/// design — so the content hash must not move across a supersession.
#[tokio::test]
async fn a_sealed_row_hash_is_unchanged_after_a_supersession() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    fx.seal().await;
    let victim = fx.rollups().await.first().expect("a block").id.clone();
    let before: BTreeMap<String, String> = fx
        .ledger
        .0
        .row_hashes(HashScope::Rollups)
        .await
        .expect("a read")
        .into_iter()
        .map(|h| (h.id, h.hash))
        .collect();

    fx.summarizer
        .supersede(&SupersedeRequest {
            block: victim.clone(),
            reason: "the recap missed the decision".into(),
            at: base() + chrono::Duration::days(2),
            attribution: Attribution::System,
        })
        .await
        .expect("a supersession");

    let after: Vec<bough_plugin_ledger::RowHash> = fx
        .ledger
        .0
        .row_hashes(HashScope::Rollups)
        .await
        .expect("a read");
    let row = after
        .iter()
        .find(|h| h.id == victim.as_str())
        .expect("the superseded row is still there");
    assert_eq!(
        Some(&row.hash),
        before.get(victim.as_str()),
        "the sealed row's content changed"
    );
    assert!(
        row.superseded_by.is_some(),
        "the one permitted write did happen"
    );
    for (id, hash) in &before {
        let still = after.iter().find(|h| &h.id == id).expect("still present");
        assert_eq!(&still.hash, hash, "`{id}` changed and should not have");
    }
}

#[tokio::test]
async fn superseded_by_is_set_once_and_a_second_supersession_is_refused() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    fx.seal().await;
    let victim = fx.rollups().await.first().expect("a block").id.clone();
    let req = |at| SupersedeRequest {
        block: victim.clone(),
        reason: "the recap missed the decision".into(),
        at,
        attribution: Attribution::System,
    };

    let first = fx
        .summarizer
        .supersede(&req(base() + chrono::Duration::days(2)))
        .await
        .expect("the first supersession");
    assert_eq!(first.old, victim);
    assert_ne!(first.new, victim);

    let err = fx
        .summarizer
        .supersede(&req(base() + chrono::Duration::days(3)))
        .await
        .expect_err("a second supersession of the same block is refused");
    match err {
        RollupsError::AlreadySuperseded(old, by) => {
            assert_eq!(old, victim);
            assert_eq!(by, first.new);
        }
        other => panic!("wrong refusal: {other}"),
    }

    // And a block this provider did not seal is not superseded at all: supersession is namespaced.
    let foreign = fx
        .summarizer
        .supersede(&SupersedeRequest {
            block: RollupId::new("old-feed:nodes:7"),
            reason: "not ours".into(),
            at: base(),
            attribution: Attribution::System,
        })
        .await
        .expect_err("a bridge block is refused");
    assert!(matches!(foreign, RollupsError::NotOurs(_)), "{foreign}");
}

// ---- live -------------------------------------------------------------------------------------

/// The recap prompt, judged by the only standard that matters: would a human keep this?
///
/// `set -a; . ~/.bough/env; set +a; BOUGH_LIVE=1 cargo test -p bough-plugin-rollups-summarizer \
///   --test seal_once -- --ignored`
#[tokio::test]
#[ignore = "live: needs BOUGH_LIVE=1 and ANTHROPIC_API_KEY"]
async fn a_live_haiku_pass_seals_a_readable_block() {
    if std::env::var("BOUGH_LIVE").ok().as_deref() != Some("1") {
        eprintln!("BOUGH_LIVE is not 1; skipping");
        return;
    }
    let fx = fx_live(cfg()).await;
    fx.seed(2, 10).await;
    let report = fx.seal().await;
    assert_eq!(report.sealed.len(), 1, "{report:?}");
    assert!(report.tokens_in > 0 && report.tokens_out > 0, "{report:?}");

    let block = fx.rollups().await;
    let body: bough_plugin_rollups::TierBlock =
        serde_json::from_value(block[0].body.clone()).expect("a block body");
    eprintln!("--- the sealed recap ---\n{}\n---", body.text);
    assert!(
        body.text.chars().count() > 60,
        "haiku wrote nothing worth keeping: {:?}",
        body.text
    );
    assert!(
        body.text.chars().count() <= cfg().max_block_chars,
        "the block is over its budget"
    );
    // The index is ours, not the model's, even live.
    assert_eq!(body.evidence.len(), 10);
    assert_eq!(body.prompt_ver, cfg().prompt_ver);
}
