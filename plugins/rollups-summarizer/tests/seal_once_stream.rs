//! V1, the event-stream half: the seam's `seal_once` invariant judged over the stream the REAL
//! summarizer records, not over planted observations.
//!
//! Its own test binary on purpose: `bough_plugin_rollups::invariant`'s record is process-global,
//! so a suite that reads it must be the only writer in its process.

mod support;

use bough_plugin_rollups::invariant::{evaluate_seal_once, seen, Obs};
use bough_plugin_rollups::{Attribution, Summarizer, SupersedeRequest};
use support::*;

#[tokio::test]
async fn the_stream_a_real_pass_records_satisfies_seal_once() {
    let fx = fx(cfg(), 32).await;
    fx.seed(4, 10).await;
    let report = fx.seal().await;
    assert!(!report.sealed.is_empty());

    // A second pass records nothing, because it seals nothing.
    fx.seal().await;

    let obs = seen();
    assert_eq!(
        obs.len(),
        report.sealed.len(),
        "every sealed block is observed exactly once: {obs:?}"
    );
    assert!(obs.iter().all(|o| o.generation == 0));
    evaluate_seal_once(&obs).expect("a real pass violates seal-once");

    // A supersession is the ONE thing that may re-cover a range, and it arrives as generation 1.
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

    let obs = seen();
    let gen1: Vec<&Obs> = obs.iter().filter(|o| o.generation == 1).collect();
    assert_eq!(
        gen1.len(),
        1,
        "a supersession records one generation-1 block"
    );
    evaluate_seal_once(&obs).expect("a real supersession violates seal-once");

    // And the statement has teeth on THIS stream: replay the real generation-0 observation of the
    // superseded range and the invariant reports it.
    let replayed = obs
        .iter()
        .find(|o| o.generation == 0 && (o.from_seq, o.to_seq) == (gen1[0].from_seq, gen1[0].to_seq))
        .expect("the original observation of the superseded range")
        .clone();
    let mut planted = obs.clone();
    planted.push(replayed);
    let detail = evaluate_seal_once(&planted).expect_err("a re-seal must be reported");
    assert!(detail.contains("sealed again at generation 0"), "{detail}");
}
