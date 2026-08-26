//! §8: a digest rebuild READS sealed tiers and writes none. It supersedes the previous digest and
//! repoints `agents.digest_rollup` — which IS the identity rebuild, since §3 says identity renders
//! from the agents row plus the digest rather than being stored.

mod support;

use bough_plugin_ledger::{HashScope, RollupKind};
use bough_plugin_rollups::{Attribution, DigestBlock, DigestRequest, Summarizer};
use support::*;

fn request(from_raw: bool, day: i64) -> DigestRequest {
    DigestRequest {
        agent: agent(),
        traj: traj(),
        at: base() + chrono::Duration::days(day),
        attribution: Attribution::System,
        parents: Vec::new(),
        reconcile: false,
        from_raw,
    }
}

#[tokio::test]
async fn rebuild_digest_supersedes_and_repoints_the_agent_row() {
    let fx = fx(cfg(), 32).await;
    fx.put_agent().await;
    fx.seed(4, 10).await;
    fx.seal().await;

    let first = fx
        .summarizer
        .rebuild_digest(&request(false, 2))
        .await
        .expect("a rebuild");
    assert_eq!(first.replaced, None, "there was no digest to replace");
    assert_eq!(first.calls, 1);
    let row = fx
        .ledger
        .0
        .agent(&agent())
        .await
        .expect("a read")
        .expect("the agent row");
    assert_eq!(
        row.digest_rollup.as_ref(),
        Some(&first.digest),
        "identity renders from this pointer, so repointing it IS the rebuild"
    );

    let second = fx
        .summarizer
        .rebuild_digest(&request(false, 3))
        .await
        .expect("a second rebuild");
    assert_eq!(second.replaced.as_ref(), Some(&first.digest));
    assert_ne!(second.digest, first.digest, "generation n+1");
    assert!(second.digest.as_str().ends_with("#g1"));

    let rollups = fx.rollups().await;
    let old = rollups
        .iter()
        .find(|r| r.id == first.digest)
        .expect("the old digest is still there — nothing is deleted");
    assert_eq!(old.superseded_by.as_ref(), Some(&second.digest));
    let row = fx
        .ledger
        .0
        .agent(&agent())
        .await
        .expect("a read")
        .expect("the agent row");
    assert_eq!(row.digest_rollup.as_ref(), Some(&second.digest));

    // The block names what it was built from, and its evidence is RAW.
    let body: DigestBlock = serde_json::from_value(
        rollups
            .iter()
            .find(|r| r.id == first.digest)
            .expect("the digest")
            .body
            .clone(),
    )
    .expect("a digest body");
    assert!(!body.evidence.is_empty(), "a digest indexes raw evidence");
    assert_eq!(body.from_blocks.len(), 3, "it named the sealed tiers");
    assert_eq!(body.replaces, None);
    assert_eq!(body.prompt_ver, cfg().prompt_ver);
}

/// §8: "`/reset` rebuilds the digest … from raw evidence; sealed tiers are never re-summarized by
/// it." The reset's block therefore names NO tier — not in its prompt, and not in `from_blocks`.
#[tokio::test]
async fn a_reset_rebuild_names_no_sealed_tier() {
    let fx = fx(cfg(), 32).await;
    fx.put_agent().await;
    fx.seed(4, 10).await;
    fx.seal().await;
    let tiers = fx
        .rollups()
        .await
        .into_iter()
        .filter(|r| r.kind == RollupKind::Tier)
        .count();
    assert!(tiers > 0, "this test is vacuous with no sealed tiers");

    let report = fx
        .summarizer
        .rebuild_digest(&request(true, 2))
        .await
        .expect("a reset rebuild");
    assert_eq!(
        report.tiers_read, tiers,
        "the report still says how many tiers stand, so the count can be checked"
    );
    let body: DigestBlock = serde_json::from_value(
        fx.rollups()
            .await
            .into_iter()
            .find(|r| r.id == report.digest)
            .expect("the digest")
            .body,
    )
    .expect("a digest body");
    assert!(
        body.from_blocks.is_empty(),
        "a reset's digest is built from RAW evidence and names no tier: {:?}",
        body.from_blocks
    );
    assert!(!body.evidence.is_empty(), "and it does index the raw");
}

/// The property `/reset` rests on: a rebuild is not a re-seal.
#[tokio::test]
async fn rebuild_digest_reads_sealed_tiers_and_writes_none() {
    let fx = fx(cfg(), 32).await;
    fx.put_agent().await;
    fx.seed(4, 10).await;
    fx.seal().await;

    let tiers_before: Vec<_> = fx
        .rollups()
        .await
        .into_iter()
        .filter(|r| r.kind == RollupKind::Tier)
        .collect();
    let hashes_before: Vec<_> = fx
        .ledger
        .0
        .row_hashes(HashScope::Rollups)
        .await
        .expect("a read")
        .into_iter()
        .filter(|h| tiers_before.iter().any(|t| t.id.as_str() == h.id))
        .map(|h| (h.id, h.hash, h.superseded_by))
        .collect();

    let report = fx
        .summarizer
        .rebuild_digest(&request(true, 2))
        .await
        .expect("a rebuild");
    assert_eq!(
        report.tiers_read,
        tiers_before.len(),
        "the rebuild read every sealed tier"
    );
    assert!(report.tiers_read > 0, "and there were some to read");

    let tiers_after: Vec<_> = fx
        .rollups()
        .await
        .into_iter()
        .filter(|r| r.kind == RollupKind::Tier)
        .collect();
    assert_eq!(
        tiers_after.len(),
        tiers_before.len(),
        "a rebuild sealed a tier"
    );
    let hashes_after: Vec<_> = fx
        .ledger
        .0
        .row_hashes(HashScope::Rollups)
        .await
        .expect("a read")
        .into_iter()
        .filter(|h| tiers_before.iter().any(|t| t.id.as_str() == h.id))
        .map(|h| (h.id, h.hash, h.superseded_by))
        .collect();
    assert_eq!(
        hashes_after, hashes_before,
        "a sealed tier changed across a digest rebuild"
    );
    // The one row it DID write is a digest, and it is the only one.
    let digests: Vec<_> = fx
        .rollups()
        .await
        .into_iter()
        .filter(|r| r.kind == RollupKind::Digest)
        .collect();
    assert_eq!(digests.len(), 1);
    assert_eq!(digests[0].id, report.digest);
    assert_eq!(digests[0].tier, 0, "a digest is not fine detail");
}
