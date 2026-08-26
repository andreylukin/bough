//! Invariant: the planner refuses BEFORE the store does. A range already covered by a block in
//! this crate's namespace is never planned again — superseded blocks included, because a
//! superseded range is still sealed — and a block minted by the §14 bridge is in a foreign
//! namespace and therefore invisible to the overlap check (P4-D13).

use std::collections::BTreeSet;

use bough_plugin_ledger::{Rollup, RollupId, RollupKind, Seq, TrajId};

use crate::request::{Inputs, PlannedBlock, SealPlan, Skip, SkipReason};
use crate::window::{Cut, Window, WindowCfg};

/// The one namespace prefix this crate's blocks carry (P4-D13).
pub const TIER_NAMESPACE: &str = "tier:";

/// The tier tree's shape.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TierCfg {
    /// §3: fanout ~10. Tier k+1 reduces exactly `fanout` tier-k blocks.
    pub fanout: usize,
    /// The highest tier this deployment builds.
    pub max_tier: u8,
    /// Never seal within this many steps of the head (P4-D11).
    pub lag: usize,
    /// The tier-1 window size the coverage arithmetic is stated against. Mirrors the summarizer
    /// row's `max_window_steps`, so [`coverage`] is a total function of `TierCfg` alone.
    pub max_window_steps: usize,
}

/// The deterministic id of a tier block.
///
/// EXCLUDES `prompt_ver` on purpose (P4-D4): a prompt bump must not re-open a sealed range.
/// `gen` 0 is the original; n>0 is the nth supersession.
pub fn tier_id(traj: &TrajId, tier: u8, from: Seq, to: Seq, generation: u32) -> RollupId {
    let base = format!("{TIER_NAMESPACE}{traj}:{tier}:{}-{}", from.0, to.0);
    RollupId::new(if generation == 0 {
        base
    } else {
        format!("{base}#g{generation}")
    })
}

/// The generation encoded in a block id, or `None` when the id is not ours.
///
/// The invariant runner reads it back off the id rather than off a column, because the id is what
/// the seal-once statement is written about.
pub fn generation_of(id: &RollupId) -> Option<u32> {
    if !is_ours(id) {
        return None;
    }
    match id.as_str().split_once("#g") {
        None => Some(0),
        Some((_, n)) => n.parse().ok(),
    }
}

/// `true` iff `id` is in this crate's namespace.
///
/// Bridge blocks (`old-feed:…`) are not, and are therefore invisible to the overlap check
/// (P4-D13): the two vocabularies coexist and neither poisons the other's seal-once arithmetic.
pub fn is_ours(id: &RollupId) -> bool {
    id.as_str().starts_with(TIER_NAMESPACE)
}

/// §3: "tier k covers ~10^k steps". The exact arithmetic, so the property is a unit test rather
/// than a comment: `max_window_steps * fanout^(tier-1)`.
pub fn coverage(tier: u8, cfg: &TierCfg) -> usize {
    if tier == 0 {
        return 0;
    }
    cfg.max_window_steps
        .saturating_mul(cfg.fanout.saturating_pow(u32::from(tier - 1)))
}

/// The live (non-superseded) block covering a range at a tier, if one exists.
fn live_block(
    existing: &[Rollup],
    traj: &TrajId,
    tier: u8,
    from: Seq,
    to: Seq,
) -> Option<RollupId> {
    existing
        .iter()
        .filter(|r| {
            r.kind == RollupKind::Tier
                && r.traj == *traj
                && r.tier == tier
                && r.from_seq == from
                && r.to_seq == to
                && is_ours(&r.id)
                && r.superseded_by.is_none()
        })
        .map(|r| r.id.clone())
        .next()
}

/// The whole plan, from the ledger's own rows.
///
/// `existing` is every rollup on the trajectory, superseded ones INCLUDED — a superseded range is
/// still sealed and is never re-planned.
pub fn plan(
    existing: &[Rollup],
    windows: &[Window],
    head: Seq,
    upto: Seq,
    traj: &TrajId,
    cfg: &TierCfg,
    wcfg: &WindowCfg,
) -> SealPlan {
    // P4-D11: the lag is a floor the caller cannot lift. `upto` only ever tightens it.
    let lagged = Seq(head.0.saturating_sub(cfg.lag as u64));
    let upto = Seq(upto.0.min(lagged.0));

    // Only OUR namespace counts as sealed (P4-D13), and only tier rollups on this trajectory.
    let sealed: BTreeSet<(u8, u64, u64)> = existing
        .iter()
        .filter(|r| r.kind == RollupKind::Tier && r.traj == *traj && is_ours(&r.id))
        .map(|r| (r.tier, r.from_seq.0, r.to_seq.0))
        .collect();

    let mut blocks: Vec<PlannedBlock> = Vec::new();
    let mut skipped: Vec<Skip> = Vec::new();

    // ---- tier 1: one block per closed episode window --------------------------------------
    //
    // A child of tier 2 is any tier-1 block that EXISTS after this pass: the ones planned here
    // and the ones already sealed. A range skipped for any other reason is not a child.
    let mut children: Vec<(RollupId, Seq, Seq)> = Vec::new();
    for w in windows {
        let (from, to) = (w.from_seq, w.to_seq);
        let id = tier_id(traj, 1, from, to, 0);
        let why = if w.cut == Cut::Head || to.0 > upto.0 {
            Some(SkipReason::TooCloseToHead)
        } else if w.steps.len() < wcfg.min_steps {
            Some(SkipReason::TooShort)
        } else if sealed.contains(&(1, from.0, to.0)) {
            // The child of tier 2 is the row that EXISTS, which after a supersession is not the
            // generation-0 id: name the live one.
            let live = live_block(existing, traj, 1, from, to).unwrap_or_else(|| id.clone());
            children.push((live, from, to));
            Some(SkipReason::AlreadySealed)
        } else {
            None
        };
        match why {
            Some(why) => skipped.push(Skip {
                tier: 1,
                from_seq: from,
                to_seq: to,
                why,
            }),
            None => {
                children.push((id.clone(), from, to));
                blocks.push(PlannedBlock {
                    id,
                    tier: 1,
                    from_seq: from,
                    to_seq: to,
                    inputs: Inputs::Raw(w.steps.clone()),
                    windows: vec![w.clone()],
                });
            }
        }
    }

    // ---- tier k>1: exactly `fanout` children of tier k-1 -----------------------------------
    for tier in 2..=cfg.max_tier {
        // Everything at the layer beneath, sealed or planned, oldest first and deduped by range.
        let mut kids: Vec<(RollupId, Seq, Seq)> = existing
            .iter()
            .filter(|r| {
                r.kind == RollupKind::Tier
                    && r.traj == *traj
                    && r.tier == tier - 1
                    && is_ours(&r.id)
                    && r.superseded_by.is_none()
            })
            .map(|r| (r.id.clone(), r.from_seq, r.to_seq))
            .chain(children.iter().cloned())
            .collect();
        kids.sort_by_key(|(_, from, to)| (from.0, to.0));
        kids.dedup_by_key(|(_, from, to)| (from.0, to.0));

        let mut next: Vec<(RollupId, Seq, Seq)> = Vec::new();
        for chunk in kids.chunks(cfg.fanout.max(2)) {
            let from = chunk[0].1;
            let to = chunk[chunk.len() - 1].2;
            if chunk.len() < cfg.fanout.max(2) {
                skipped.push(Skip {
                    tier,
                    from_seq: from,
                    to_seq: to,
                    why: SkipReason::NotEnoughChildren,
                });
                continue;
            }
            let id = tier_id(traj, tier, from, to, 0);
            if to.0 > upto.0 {
                skipped.push(Skip {
                    tier,
                    from_seq: from,
                    to_seq: to,
                    why: SkipReason::TooCloseToHead,
                });
                continue;
            }
            if sealed.contains(&(tier, from.0, to.0)) {
                next.push((id, from, to));
                skipped.push(Skip {
                    tier,
                    from_seq: from,
                    to_seq: to,
                    why: SkipReason::AlreadySealed,
                });
                continue;
            }
            next.push((id.clone(), from, to));
            blocks.push(PlannedBlock {
                id,
                tier,
                from_seq: from,
                to_seq: to,
                inputs: Inputs::Blocks(chunk.iter().map(|(id, _, _)| id.clone()).collect()),
                // A tier k>1 block reduces CHILDREN, not windows; its windows are the children's.
                windows: Vec::new(),
            });
        }
        children = next;
        if children.is_empty() {
            break;
        }
    }

    SealPlan {
        traj: traj.clone(),
        head,
        upto,
        blocks,
        skipped,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::window::fixture::{at, run};
    use crate::window::windows;
    use chrono::Duration;

    fn tcfg(fanout: usize, max_tier: u8, lag: usize) -> TierCfg {
        TierCfg {
            fanout,
            max_tier,
            lag,
            max_window_steps: 4,
        }
    }

    fn wcfg() -> WindowCfg {
        WindowCfg {
            gap: Duration::seconds(3600),
            max_steps: 4,
            min_steps: 1,
        }
    }

    fn traj() -> TrajId {
        TrajId::new("t")
    }

    /// A sealed row as the ledger hands it back.
    fn sealed(id: RollupId, tier: u8, from: u64, to: u64, superseded: Option<&str>) -> Rollup {
        Rollup {
            id,
            traj: traj(),
            kind: RollupKind::Tier,
            tier,
            from_seq: Seq(from),
            to_seq: Seq(to),
            src_trajs: vec![],
            body: serde_json::json!({}),
            notable_refs: Default::default(),
            prompt_ver: "recap-1".into(),
            sealed_at: at(0),
            superseded_by: superseded.map(RollupId::new),
        }
    }

    #[test]
    fn tier_one_covers_one_episode_window() {
        let ws = windows(&run(20), &wcfg());
        let p = plan(
            &[],
            &ws,
            Seq(20),
            Seq(20),
            &traj(),
            &tcfg(10, 1, 0),
            &wcfg(),
        );
        assert_eq!(p.blocks.len(), 4, "five windows, the last one still open");
        for (b, w) in p.blocks.iter().zip(ws.iter()) {
            assert_eq!(b.tier, 1);
            assert_eq!(b.windows, vec![w.clone()], "one window per tier-1 block");
            assert_eq!(b.inputs, Inputs::Raw(w.steps.clone()));
        }
    }

    #[test]
    fn tier_k_reduces_exactly_fanout_children() {
        // 24 steps at 4 per window = 6 windows, the last open ⇒ 5 sealable tier-1 blocks.
        let ws = windows(&run(24), &wcfg());
        let p = plan(&[], &ws, Seq(24), Seq(24), &traj(), &tcfg(2, 3, 0), &wcfg());
        let t2: Vec<&PlannedBlock> = p.blocks.iter().filter(|b| b.tier == 2).collect();
        assert_eq!(
            t2.len(),
            2,
            "five tier-1 children at fanout 2 ⇒ two tier-2 blocks"
        );
        for b in &t2 {
            match &b.inputs {
                Inputs::Blocks(kids) => assert_eq!(kids.len(), 2, "exactly `fanout` children"),
                other => panic!("tier 2 reduces blocks, not {other:?}"),
            }
        }
        let t3: Vec<&PlannedBlock> = p.blocks.iter().filter(|b| b.tier == 3).collect();
        assert_eq!(t3.len(), 1, "the two tier-2 blocks reduce once more");
        // The odd child out is reported, never silently dropped.
        assert!(p
            .skipped
            .iter()
            .any(|s| s.tier == 2 && s.why == SkipReason::NotEnoughChildren));
    }

    #[test]
    fn coverage_is_max_window_steps_times_fanout_to_the_k_minus_one() {
        let cfg = TierCfg {
            fanout: 10,
            max_tier: 3,
            lag: 20,
            max_window_steps: 10,
        };
        assert_eq!(coverage(1, &cfg), 10);
        assert_eq!(coverage(2, &cfg), 100);
        assert_eq!(coverage(3, &cfg), 1000, "§3: tier k covers ~10^k steps");
        assert_eq!(coverage(0, &cfg), 0, "there is no tier 0");
    }

    #[test]
    fn a_range_already_sealed_is_never_planned_again() {
        let ws = windows(&run(20), &wcfg());
        let first = &ws[0];
        let existing = vec![sealed(
            tier_id(&traj(), 1, first.from_seq, first.to_seq, 0),
            1,
            first.from_seq.0,
            first.to_seq.0,
            None,
        )];
        let p = plan(
            &existing,
            &ws,
            Seq(20),
            Seq(20),
            &traj(),
            &tcfg(10, 1, 0),
            &wcfg(),
        );
        assert!(
            !p.blocks.iter().any(|b| b.from_seq == first.from_seq),
            "a sealed range is never re-planned"
        );
        assert!(p
            .skipped
            .iter()
            .any(|s| s.from_seq == first.from_seq && s.why == SkipReason::AlreadySealed));
    }

    #[test]
    fn a_superseded_block_still_counts_as_sealed() {
        let ws = windows(&run(20), &wcfg());
        let first = &ws[0];
        let existing = vec![sealed(
            tier_id(&traj(), 1, first.from_seq, first.to_seq, 0),
            1,
            first.from_seq.0,
            first.to_seq.0,
            Some("tier:t:1:1-4#g1"),
        )];
        let p = plan(
            &existing,
            &ws,
            Seq(20),
            Seq(20),
            &traj(),
            &tcfg(10, 1, 0),
            &wcfg(),
        );
        assert!(
            p.skipped
                .iter()
                .any(|s| s.from_seq == first.from_seq && s.why == SkipReason::AlreadySealed),
            "a superseded range is still sealed; supersession is the relief valve, not a re-open"
        );
    }

    #[test]
    fn a_bridge_namespace_block_does_not_block_a_plan() {
        let ws = windows(&run(20), &wcfg());
        let first = &ws[0];
        // The §14 bridge borrows a foreign row id into the seq namespace (P4-D13).
        let existing = vec![sealed(
            RollupId::new("old-feed:jungler:17"),
            1,
            first.from_seq.0,
            first.to_seq.0,
            None,
        )];
        let p = plan(
            &existing,
            &ws,
            Seq(20),
            Seq(20),
            &traj(),
            &tcfg(10, 1, 0),
            &wcfg(),
        );
        assert!(
            p.blocks.iter().any(|b| b.from_seq == first.from_seq),
            "a foreign-namespace block is invisible to the overlap check"
        );
    }

    #[test]
    fn nothing_within_seal_lag_steps_of_the_head_is_planned() {
        let ws = windows(&run(20), &wcfg());
        let p = plan(
            &[],
            &ws,
            Seq(20),
            Seq(20),
            &traj(),
            &tcfg(10, 1, 9),
            &wcfg(),
        );
        assert_eq!(p.upto, Seq(11), "the lag is a floor the caller cannot lift");
        assert!(
            p.blocks.iter().all(|b| b.to_seq.0 <= 11),
            "no block reaches into the lag window"
        );
        assert!(p
            .skipped
            .iter()
            .any(|s| s.why == SkipReason::TooCloseToHead));
    }

    #[test]
    fn tier_id_is_deterministic_and_excludes_prompt_ver() {
        let a = tier_id(&traj(), 2, Seq(1), Seq(40), 0);
        let b = tier_id(&traj(), 2, Seq(1), Seq(40), 0);
        assert_eq!(a, b, "the same range mints the same id");
        assert_eq!(a.as_str(), "tier:t:2:1-40");
        assert!(
            !a.as_str().contains("recap"),
            "P4-D4: a prompt bump must not re-open a sealed range"
        );
        assert!(is_ours(&a));
        assert!(!is_ours(&RollupId::new("old-feed:jungler:17")));
    }

    #[test]
    fn a_supersession_id_carries_the_next_generation() {
        let g0 = tier_id(&traj(), 1, Seq(1), Seq(4), 0);
        let g1 = tier_id(&traj(), 1, Seq(1), Seq(4), 1);
        assert_eq!(g1.as_str(), "tier:t:1:1-4#g1");
        assert_ne!(g0, g1);
        assert_eq!(generation_of(&g0), Some(0));
        assert_eq!(generation_of(&g1), Some(1));
        assert_eq!(generation_of(&RollupId::new("old-feed:x:1")), None);
    }

    #[test]
    fn the_plan_is_total_every_candidate_is_planned_or_skipped() {
        let ws = windows(&run(24), &wcfg());
        let p = plan(&[], &ws, Seq(24), Seq(24), &traj(), &tcfg(2, 3, 0), &wcfg());
        let tier1 = p.blocks.iter().filter(|b| b.tier == 1).count()
            + p.skipped.iter().filter(|s| s.tier == 1).count();
        assert_eq!(
            tier1,
            ws.len(),
            "every window is planned or skipped, never dropped"
        );
        // And no range is BOTH.
        for b in &p.blocks {
            assert!(
                !p.skipped
                    .iter()
                    .any(|s| s.tier == b.tier && s.from_seq == b.from_seq && s.to_seq == b.to_seq),
                "a range is planned or skipped, not both"
            );
        }
    }
}
