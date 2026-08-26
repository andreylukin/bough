//! Invariant under test: `nodes.summary` and `lane_story` become INTERIM TIER-1 BLOCKS that seal
//! ONCE and reach the agent through the ordinary tiers band — which is the whole of §17's
//! "softening the no-tiers window" (§2.6).

mod common;

use std::sync::Arc;

use bough_plugin_ledger::{RollupKind, RollupQuery};
use bough_plugin_old_feed_adapter::state::{Watermark, WatermarkStore};
use bough_plugin_old_feed_adapter::PROMPT_VER;
use bough_plugin_projection::{AssembleRequest, Projector};
use bough_plugin_projection_assembler::{Assembler, AssemblerConfig};
use common::{at, Fx, Which};

async fn tiers(fx: &Fx) -> Vec<bough_plugin_ledger::Rollup> {
    fx.ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![common::traj()],
            kind: Some(RollupKind::Tier),
            ..Default::default()
        })
        .await
        .expect("a read")
}

#[tokio::test]
async fn nodes_summary_rows_seal_as_tier_one_rollups() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_jungler(&fx.jungler_db);
    let _sol = fx.sol_agent().await;

    fx.feed(fx.cfg()).sweep_at(at()).await.expect("a sweep");

    let node_blocks: Vec<_> = tiers(&fx)
        .await
        .into_iter()
        .filter(|r| r.id.as_str().starts_with("old-feed:node:"))
        .collect();
    assert_eq!(
        node_blocks.len(),
        1,
        "the node with an EMPTY summary seals nothing"
    );
    let block = &node_blocks[0];
    assert_eq!(block.tier, 1);
    assert_eq!(block.kind, RollupKind::Tier);
    assert_eq!(block.prompt_ver, PROMPT_VER);
    assert_eq!(
        block.body.get("text").and_then(|v| v.as_str()),
        Some("the rebuild is under way")
    );
    assert!(
        block.notable_refs.is_empty(),
        "empty notable_refs is `notable to everyone` (P1-D13); anything else filters the bridge \
         straight back out of the band"
    );
}

#[tokio::test]
async fn lane_story_sections_seal_in_ord_order() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_jungler(&fx.jungler_db);
    let _sol = fx.sol_agent().await;

    fx.feed(fx.cfg()).sweep_at(at()).await.expect("a sweep");

    // The fixture stores the sections in the WRONG id order on purpose: row 1 is `ord` 2.
    let story: Vec<String> = tiers(&fx)
        .await
        .into_iter()
        .filter(|r| r.id.as_str().starts_with("old-feed:story:"))
        .map(|r| {
            r.body
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect();
    assert_eq!(story, vec!["first".to_string(), "second".to_string()]);
}

#[tokio::test]
async fn a_second_sweep_seals_nothing_again() {
    let fx = Fx::new(Which::Memory).await;
    common::standard_jungler(&fx.jungler_db);
    let _sol = fx.sol_agent().await;

    fx.feed(fx.cfg()).sweep_at(at()).await.expect("a sweep");
    let first = tiers(&fx).await.len();
    assert_eq!(first, 3, "one node block and two story blocks");

    // Both a plain restart AND a rolled-back watermark: seal-once is a data property, not a
    // consequence of the watermark alone (§3).
    fx.feed(fx.cfg()).sweep_at(at()).await.expect("a re-sweep");
    let store = WatermarkStore::open(&fx.state_db).expect("the adapter's own db");
    store
        .set("jungler.nodes", Watermark::default(), at())
        .expect("a rollback");
    store
        .set("jungler.lane_story", Watermark::default(), at())
        .expect("a rollback");
    drop(store);
    fx.feed(fx.cfg())
        .sweep_at(at())
        .await
        .expect("a re-sweep after the rollback");

    assert_eq!(tiers(&fx).await.len(), first, "nothing sealed twice");
}

/// Both ledger providers, because a projection claim that holds on one store and not the other is
/// not a claim about the projection (Phase 1's rule).
#[tokio::test]
async fn the_projection_shows_them_in_the_tiers_band() {
    for which in [Which::Memory, Which::Sqlite] {
        let fx = Fx::new(which).await;
        common::standard_jungler(&fx.jungler_db);
        let _sol = fx.sol_agent().await;
        fx.feed(fx.cfg()).sweep_at(at()).await.expect("a sweep");

        let cfg = AssemblerConfig {
            budget_tokens: 100_000,
            headroom: 0.6,
            tail_steps: 20,
            tail_floor_steps: 4,
            mail_newest_n: 5,
            max_tiers: 3,
            file_view_dir: fx.dir.path().join("views"),
        };
        let assembler = Assembler::new(Arc::new(cfg), fx.ledger.clone(), fx.ctx.clone());
        let out = assembler
            .assemble(&AssembleRequest {
                as_of: None,
                agent: common::sol(),
                wake: None,
                at: at(),
                budget: None,
            })
            .await
            .unwrap_or_else(|e| panic!("assemble on {which:?}: {e}"))
            .to_text();

        assert!(
            out.contains("Tier 1 summary"),
            "{which:?}: the bridge's blocks arrive in the tiers band:\n{out}"
        );
        assert!(
            out.contains("the rebuild is under way"),
            "{which:?}: the node summary is what the band renders:\n{out}"
        );
        let first = out.find("the first chapter").expect("the first chapter");
        let second = out.find("the second chapter").expect("the second chapter");
        assert!(first < second, "{which:?}: the story reads in ord order");
    }
}
