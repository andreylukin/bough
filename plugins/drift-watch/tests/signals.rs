//! §8's stability signals, over a REAL store: computing them is a read, and a read appends
//! nothing. The arithmetic itself is unit-tested in `signals.rs` without a ledger; what this file
//! pins is the one property only a store can witness.

mod common;

use bough_plugin_drift_watch::{DriftFlag, SignalState};
use bough_plugin_ledger::{HashScope, Seq};

#[tokio::test]
async fn signals_are_read_only_and_append_nothing() {
    let h = common::harness().await;
    common::seed_trajectory(&h).await;

    let before_steps = common::all_steps(&h).await;
    let before_hashes = common::hashes(&h, HashScope::All).await;
    assert!(!before_steps.is_empty(), "the fixture seeded a trajectory");

    let signals = h
        .drift
        .signals(&common::agent(), common::at())
        .await
        .expect("the signals compute");

    // What it measured: the seeded window is four thoughts and four calls over two tools.
    assert_eq!(signals.agent, common::agent());
    assert_eq!(signals.window.to, Seq(before_steps.len() as u64));
    assert_eq!(signals.thought_len.n, 4, "{signals:?}");
    assert_eq!(signals.samples, 8, "{signals:?}");
    assert_eq!(
        signals
            .tool_use
            .iter()
            .map(|t| t.tool.as_str())
            .collect::<Vec<_>>(),
        vec!["bash", "read"],
        "most-used first: {signals:?}"
    );
    assert!(
        signals.tool_entropy > 0.0,
        "two tools have spread: {signals:?}"
    );
    assert!(
        matches!(signals.claim_rejection, SignalState::Inactive { .. }),
        "the seeded window decides no claim, so the rate is not a number: {signals:?}"
    );
    assert!(
        !signals.flags.contains(&DriftFlag::TooFewSamples),
        "eight samples clears the fixture's floor of four: {signals:?}"
    );

    // THE POINT: not one row was written, and not one row changed.
    let after_steps = common::all_steps(&h).await;
    assert_eq!(
        before_steps.len(),
        after_steps.len(),
        "computing signals appended a step"
    );
    assert_eq!(
        before_steps, after_steps,
        "computing signals changed a step"
    );
    assert_eq!(
        before_hashes,
        common::hashes(&h, HashScope::All).await,
        "computing signals changed a row hash"
    );

    // And it is idempotent: the same window read twice is the same answer.
    let again = h
        .drift
        .signals(&common::agent(), common::at())
        .await
        .expect("the signals compute again");
    assert_eq!(signals, again);
}

#[tokio::test]
async fn an_agent_with_no_row_is_refused_rather_than_measured() {
    let h = common::harness().await;
    let err = h
        .drift
        .signals(&bough_plugin_ledger::AgentName::new("nobody"), common::at())
        .await
        .expect_err("an agent with no `agents` row has no trajectory to measure");
    assert!(err.to_string().contains("not in the registry"), "{err}");
}
