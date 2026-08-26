//! Invariant (§5, §17 Phase 4): a projection over REAL sealed tiers is deterministic and
//! provider-independent, byte for byte.
//!
//! Phase 1 wrote the golden suite against a ledger with zero rollups, because none existed yet:
//! `zero_rollups.txt` is that hole, written down. This is the case Phase 1 could not write. The
//! blocks are sealed with the `bough-plugin-rollups` vocabulary (`TierBlock`, `DigestBlock`), and
//! one of them is expired by an appended marker, so the golden pins the §8 rule too.
//!
//! DEVIATION from the WP-5 brief, named on purpose: the brief seals the fixture by driving a
//! scripted `rollups-summarizer` over `llm-replay`. The blocks here are sealed through the ledger
//! seam directly with the same vocabulary. What this file is ABOUT is the projector's reading of
//! sealed rows, and going through the summarizer would make the byte-stability of this golden
//! depend on a prompt, a transcript and another work package's crate. `rollups-summarizer`'s own
//! tests are what prove the summarizer writes rows of this shape.

mod support;

use bough_plugin_ledger::{RollupId, StepId};
use bough_plugin_rollups::Beneath;
use std::path::PathBuf;
use support::*;

const CASE: &str = "real_tiers";

/// The one fixture, seeded identically on either provider.
async fn seed(h: &Harness) {
    h.put_agent(Some("r-digest")).await;
    h.digest(
        "r-digest",
        1,
        12,
        "sol keeps the tree green and ships behind gates.",
    )
    .await;
    h.pin(
        "p1",
        "gates before commit",
        "`make gates` must be green before every commit",
    )
    .await;
    for n in 1..=12 {
        h.note(
            &format!("s{n}"),
            if n % 3 == 0 { "w2" } else { "w1" },
            &format!("verbatim step number {n}"),
        )
        .await;
    }
    h.mail("m1", "ordinary", "andrey", "look at the tiers band")
        .await;

    // Tier 1 over each half, tier 2 over both, tier 3 over the whole run: the tree §3 describes,
    // each block naming the layer beneath it.
    h.tier(
        "r-t1a",
        1,
        1,
        6,
        "sol opened the trajectory and worked through the first six steps.",
        &[MINE],
        Beneath::Raw {
            steps: (1..=6).map(|n| StepId::new(format!("s{n}"))).collect(),
        },
    )
    .await;
    h.tier(
        "r-t1b",
        1,
        7,
        12,
        "sol finished the run and left the tree green.",
        &[],
        Beneath::Raw {
            steps: (7..=12).map(|n| StepId::new(format!("s{n}"))).collect(),
        },
    )
    .await;
    h.tier(
        "r-t1c",
        1,
        1,
        12,
        "a block about somebody else's repository entirely.",
        &["gh:other/repo#99"],
        Beneath::Raw { steps: Vec::new() },
    )
    .await;
    h.tier(
        "r-t2",
        2,
        1,
        12,
        "one run: sol worked the trajectory end to end and kept the gates green.",
        &[],
        Beneath::Blocks {
            rollups: vec![RollupId::new("r-t1a"), RollupId::new("r-t1b")],
        },
    )
    .await;
    h.tier(
        "r-t3",
        3,
        1,
        12,
        "a block that was wrong about the range and has been expired.",
        &[],
        Beneath::Blocks {
            rollups: vec![RollupId::new("r-t2")],
        },
    )
    .await;

    // §8: the appended marker the projector honours — one tier block and one raw step.
    h.expire(
        "x1",
        &["rollup:r-t3", "step:s5"],
        "the tier misread the range; the step described a file that is gone",
    )
    .await;
}

async fn run(which: Which) -> String {
    let h = Harness::open(which);
    seed(&h).await;
    h.assemble(cfg(100_000)).await.to_text()
}

fn golden_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{CASE}.txt"))
}

/// The Phase-1 mechanism, unchanged: compare, or rewrite under `UPDATE_GOLDEN=1`.
fn assert_golden(got: &str) {
    let path = golden_path();
    if std::env::var("UPDATE_GOLDEN").ok().as_deref() == Some("1") {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, got.as_bytes()).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        panic!(
            "golden {} is missing ({e}); rerun with UPDATE_GOLDEN=1 to write it",
            path.display()
        )
    });
    assert_eq!(got, want, "golden `{CASE}` drifted");
}

#[tokio::test]
async fn a_projection_over_real_sealed_tiers_matches_its_golden() {
    let sqlite = run(Which::Sqlite).await;
    let memory = run(Which::Memory).await;
    assert_eq!(
        sqlite, memory,
        "`{CASE}` differs between ledger-sqlite and ledger-memory"
    );
    assert_golden(&sqlite);
}

// ---- what the bytes MEAN ------------------------------------------------------------------------
//
// A golden proves the bytes did not move; these assert what they say, so a careless
// `UPDATE_GOLDEN=1` cannot quietly rewrite the rule along with the text.

#[tokio::test]
async fn the_tiers_band_is_coarse_to_fine_and_filtered_and_expired() {
    let text = run(Which::Memory).await;
    let order: Vec<usize> = ["## Tier 2 summary", "## Tier 1 summary", "## Recent steps"]
        .iter()
        .map(|h| {
            text.find(h)
                .unwrap_or_else(|| panic!("`{h}` is missing from:\n{text}"))
        })
        .collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted, "the tiers are not coarse to fine");

    assert!(
        !text.contains("## Tier 3 summary"),
        "the expired tier-3 block still has a band:\n{text}"
    );
    assert!(
        !text.contains("somebody else's repository"),
        "a block whose notable_refs miss the agent reached the draft:\n{text}"
    );
    assert!(
        text.contains("sol opened the trajectory") && text.contains("left the tree green"),
        "the surviving tier-1 blocks are both rendered:\n{text}"
    );
    assert!(
        !text.contains("verbatim step number 5"),
        "the expired raw step is still in the verbatim tail:\n{text}"
    );
}
