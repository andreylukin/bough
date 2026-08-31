//! V1, the runtime half: the seam's `seal_once` invariant judged over the sealed rows a REAL pass
//! leaves in the store, not over planted observations and not over a record the provider kept of
//! its own behaviour.
//!
//! `bough_plugin_rollups::invariant::sealed_blocks` is the same reader the kernel's check uses, so
//! this suite judges exactly the relation the runner judges.

use crate::support;

use bough_plugin_rollups::invariant::{evaluate_seal_once, sealed_blocks};
use bough_plugin_rollups::{Attribution, Summarizer, SupersedeRequest};
use support::*;

#[tokio::test]
async fn the_store_a_real_pass_leaves_satisfies_seal_once() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    let report = fx.seal().await;
    assert!(!report.sealed.is_empty());

    // A second pass changes nothing, because it seals nothing.
    fx.seal().await;

    let obs = sealed_blocks(&fx.ledger).await.expect("the store reads");
    assert_eq!(
        obs.len(),
        report.sealed.len(),
        "every sealed block is in the store exactly once: {obs:?}"
    );
    assert!(obs.iter().all(|o| o.generation == 0));
    assert!(
        obs.iter().all(|o| o.superseded_by.is_none()),
        "nothing is superseded before a supersession"
    );
    evaluate_seal_once(&obs).expect("a real pass violates seal-once");

    // A supersession is the ONE thing that may re-cover a range: generation 1, LINKED.
    let victim = fx.rollups().await.first().expect("a block").id.clone();
    fx.summarizer
        .supersede(&SupersedeRequest {
            block: victim.clone(),
            reason: "the recap missed the decision".into(),
            at: base() + chrono::Duration::days(2),
            attribution: Attribution::System,
        })
        .await
        .expect("a supersession");

    let obs = sealed_blocks(&fx.ledger).await.expect("the store reads");
    let gen1: Vec<_> = obs.iter().filter(|o| o.generation == 1).collect();
    assert_eq!(gen1.len(), 1, "a supersession seals one generation-1 block");
    let gen0 = obs
        .iter()
        .find(|o| o.rollup == victim)
        .expect("the superseded block is still in the store");
    assert_eq!(
        gen0.superseded_by.as_ref(),
        Some(&gen1[0].rollup),
        "the replaced block LINKS its replacement; that link is the set-once write"
    );
    evaluate_seal_once(&obs).expect("a real supersession violates seal-once");

    // And the statement has teeth on THIS relation: unlink the real supersession and the
    // invariant reports two live blocks over one range.
    let mut planted = obs.clone();
    for o in &mut planted {
        if o.rollup == victim {
            o.superseded_by = None;
        }
    }
    let detail = evaluate_seal_once(&planted).expect_err("an unlinked replacement is reported");
    assert!(detail.contains("still live"), "{detail}");
}
