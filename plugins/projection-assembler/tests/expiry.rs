//! Invariant (§8): stale evidence expires by an APPENDED marker the projector HONOURS — and by
//! nothing else. No sealed row is edited, no raw step is deleted; the marker is a step like any
//! other, and the projector reads it.
//!
//! Three bands honour it (tail, tiers, digest) and two deliberately do not (pins, mail). The two
//! non-edits get tests of their own, because "we chose not to filter here" is a decision that only
//! a test can hold.

mod support;

use bough_plugin_rollups::Beneath;
use support::*;

fn raw() -> Beneath {
    Beneath::Raw { steps: Vec::new() }
}

/// A trajectory with a tail, a digest, a tier, a pin and a piece of mail.
async fn seeded() -> Harness {
    let h = Harness::open(Which::Memory);
    h.put_agent(Some("r-digest")).await;
    h.digest("r-digest", 1, 6, "the standing digest, in a sentence")
        .await;
    h.pin("p1", "the standing pin", "never expired, only superseded")
        .await;
    for n in 1..=6 {
        h.note(&format!("s{n}"), "w1", &format!("verbatim step number {n}"))
            .await;
    }
    h.mail("m1", "ordinary", "andrey", "still unconsumed").await;
    h.tier("r-t1", 1, 1, 6, "the fine tier over 1..6", &[], raw())
        .await;
    h
}

#[tokio::test]
async fn an_expired_step_leaves_the_verbatim_tail() {
    let h = seeded().await;
    let before = h.assemble(cfg(100_000)).await;
    assert!(body(&before, "tail")
        .unwrap()
        .contains("verbatim step number 3"));

    h.expire("x1", &["step:s3"], "the file it described is gone")
        .await;

    let after = h.assemble(cfg(100_000)).await;
    let tail = body(&after, "tail").expect("the tail survives");
    assert!(
        !tail.contains("verbatim step number 3"),
        "the expired step is still in the tail:\n{tail}"
    );
    assert!(
        tail.contains("verbatim step number 4"),
        "expiry took more than it was told to:\n{tail}"
    );
    // The row itself is untouched: expiry is a projection rule, not a delete (§8).
    assert!(
        h.steps().await.iter().any(|s| s.id.as_str() == "s3"),
        "the raw step was deleted rather than expired"
    );
    assert!(
        !after.cites.steps.iter().any(|s| s.as_str() == "s3"),
        "an expired step must not be cited by the projection either"
    );
}

/// §5's floor is a floor over what the model can actually SEE: the window rung 2 shrinks is the
/// one the expiry filter already ran over, so the floor counts SURVIVING steps.
#[tokio::test]
async fn the_tail_floor_counts_surviving_steps() {
    let h = Harness::open(Which::Memory);
    h.put_agent(None).await;
    for n in 1..=6 {
        h.note(&format!("s{n}"), "w1", &format!("verbatim step number {n}"))
            .await;
    }
    // One marker, three targets: the tail window is seven rows and four of them survive.
    h.expire("x1", &["step:s1", "step:s2", "step:s3"], "stale")
        .await;

    let rows = |a: &bough_plugin_projection::Assembled| {
        body(a, "tail")
            .map(|b| b.lines().filter(|l| l.starts_with("- #")).count())
            .unwrap_or(0)
    };
    let floor = cfg(0).tail_floor_steps;
    let roomy = h.assemble(cfg(100_000)).await;
    assert_eq!(
        rows(&roomy),
        4,
        "the window is seven rows, three of them expired"
    );

    // Under pressure the ladder shrinks the SURVIVING window to the floor and stops there — the
    // model never sees fewer than `tail_floor_steps` rows it is allowed to see.
    for budget in [200, 120, 60, 20] {
        let n = rows(&h.assemble(cfg(budget)).await);
        assert!(
            n >= floor,
            "budget {budget}: the tail was cut to {n} surviving rows, below the floor of {floor}"
        );
    }
    assert_eq!(
        rows(&h.assemble(cfg(20)).await),
        floor,
        "at the tightest budget the tail sits exactly on its floor"
    );
}

#[tokio::test]
async fn an_expired_tier_block_leaves_the_tiers_band() {
    let h = seeded().await;
    h.tier("r-t2", 2, 1, 6, "the coarse tier over 1..6", &[], raw())
        .await;
    let before = h.assemble(cfg(100_000)).await;
    assert!(body(&before, "tier-254").is_some(), "tier 1 renders");

    h.expire(
        "x1",
        &["rollup:r-t1"],
        "the block was wrong about the range",
    )
    .await;

    let after = h.assemble(cfg(100_000)).await;
    assert!(
        body(&after, "tier-254").is_none(),
        "the expired tier-1 band is still rendered: {:?}",
        ids(&after)
    );
    assert!(
        body(&after, "tier-253").is_some(),
        "expiry took a block it was not told to: {:?}",
        ids(&after)
    );
    // The sealed row is IMMUTABLE (§3): the marker did not edit it.
    let row = h.rollup("r-t1").await.expect("the sealed row still exists");
    assert_eq!(row.prompt_ver, "r4.1");
    assert!(row.superseded_by.is_none(), "expiry is not supersession");
}

#[tokio::test]
async fn an_expired_digest_renders_nothing() {
    let h = seeded().await;
    assert!(body(&h.assemble(cfg(100_000)).await, "digest").is_some());

    h.expire("x1", &["rollup:r-digest"], "rebuilt from raw evidence")
        .await;

    let after = h.assemble(cfg(100_000)).await;
    assert!(
        body(&after, "digest").is_none(),
        "an expired digest still rendered: {:?}",
        ids(&after)
    );
    // Nothing else moved: identity still names the pointer, because `agents` is mutable config
    // and the marker is a step — the projector reconciles them, it does not rewrite either.
    assert!(body(&after, "identity")
        .unwrap()
        .contains("digest: r-digest"));
}

/// §3, V7: a pin's only relief valve is supersession. A marker naming one is DATA the pins band
/// never consults — deliberately, so that reconsolidation can never quietly drop a standing rule.
#[tokio::test]
async fn an_expiry_marker_naming_a_pin_is_ignored() {
    let h = seeded().await;
    h.expire(
        "x1",
        &["step:p1"],
        "an expiry pass that should not reach a pin",
    )
    .await;

    let after = h.assemble(cfg(100_000)).await;
    let pins = body(&after, "pins").expect("the pins band survives");
    assert!(
        pins.contains("never expired, only superseded"),
        "an expiry marker reached a pin:\n{pins}"
    );
    assert!(
        after.cites.steps.iter().any(|s| s.as_str() == "p1"),
        "the pin is still cited"
    );
}

/// §5: unconsumed mail has its own consumption mechanism — the union of the `wake/end` sets. An
/// expiry marker must never silently un-deliver a message.
#[tokio::test]
async fn an_expiry_marker_naming_mail_is_ignored() {
    let h = seeded().await;
    h.expire(
        "x1",
        &["step:m1"],
        "an expiry pass that should not reach mail",
    )
    .await;

    let after = h.assemble(cfg(100_000)).await;
    let mail = body(&after, "mail").expect("the mail band survives");
    assert!(
        mail.contains("still unconsumed"),
        "an expiry marker un-delivered mail:\n{mail}"
    );
}
