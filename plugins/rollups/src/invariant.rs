//! §0.2 runtime invariants for the rollups seam. Both are returned by the PROVIDERS'
//! `Plugin::invariants()`; the Definition owns the statement so two providers cannot disagree
//! about what "sealed once" means.
//!
//! Both read the AUTHORITATIVE relation the seam owns — the `rollups` rows in the store — and not
//! a record the provider kept of its own behaviour. A provider that seals a block without telling
//! anyone is exactly the case an invariant has to catch, so the check cannot be built out of what
//! the provider chose to report: it must be built out of what is in the store. That also brings
//! blocks NO provider in this process sealed under the same statement — the old-feed adapter's
//! interim tier-1 rollups, and rows an earlier run left behind.
//!
//! 1. **`seal_once`** — for every `(traj, tier, from_seq, to_seq)` a tier block covers, the
//!    generations present are CONTIGUOUS with no duplicates, and every generation but the highest
//!    is superseded by the next one. A range is summarized exactly once; a supersession is
//!    generation n+1 and is linked, which is what makes `superseded_by` the one set-once write.
//! 2. **`tiers_are_an_index`** — for every sealed block, every id its `beneath` and `evidence`
//!    name resolves to a row that exists in the store at quiesce.
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] for both (P1-D14; the kernel dispatches no
//! other).

use std::collections::{BTreeMap, BTreeSet};

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{
    Ledger, LedgerError, LedgerHandle, RollupId, RollupKind, RollupQuery, Seq, StepId, TrajId,
};

/// One sealed block, as it stands in the store.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub rollup: RollupId,
    pub traj: TrajId,
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    /// The generation encoded in the block's deterministic id (0 for the original).
    pub generation: u32,
    /// The block this one was replaced by, when it has been superseded.
    pub superseded_by: Option<RollupId>,
    /// Ids the block's `beneath` and `evidence` name; `tiers_are_an_index` resolves them.
    pub beneath_steps: Vec<StepId>,
    pub beneath_rollups: Vec<RollupId>,
}

/// PURE: judge the sealed blocks in a store against the seal-once statement.
///
/// Written as a function of data so a planted violation is a unit test rather than a live run.
pub fn evaluate_seal_once(blocks: &[Obs]) -> Result<(), String> {
    let mut by_range: BTreeMap<(TrajId, u8, u64, u64), Vec<&Obs>> = BTreeMap::new();
    for b in blocks {
        by_range
            .entry((b.traj.clone(), b.tier, b.from_seq.0, b.to_seq.0))
            .or_default()
            .push(b);
    }
    for ((traj, tier, from, to), mut group) in by_range {
        group.sort_by_key(|o| o.generation);
        for (i, pair) in group.windows(2).enumerate() {
            let (prev, next) = (pair[0], pair[1]);
            if next.generation == prev.generation {
                return Err(format!(
                    "tier {tier} range {from}..{to} of `{traj}` has two blocks at generation {} \
                     (`{}` and `{}`); a range is summarized exactly once",
                    prev.generation, prev.rollup, next.rollup
                ));
            }
            if next.generation != prev.generation + 1 {
                return Err(format!(
                    "tier {tier} range {from}..{to} of `{traj}` jumps from generation {} to {} \
                     (`{}`); a supersession is generation n+1",
                    prev.generation, next.generation, next.rollup
                ));
            }
            // The link is the other half of "sealed once": a replacement that was written but
            // never linked leaves two LIVE blocks over one range, which is the state
            // `refuse_if_sealed` and `live_block` both assume cannot happen.
            match &prev.superseded_by {
                Some(by) if by == &next.rollup => {}
                Some(by) => {
                    return Err(format!(
                        "block `{}` was replaced by `{}` but its `superseded_by` names `{by}`",
                        prev.rollup, next.rollup
                    ))
                }
                None => {
                    let _ = i;
                    return Err(format!(
                        "tier {tier} range {from}..{to} of `{traj}` has a generation {} block \
                         (`{}`) while generation {} (`{}`) is still live; a replacement links the \
                         block it replaced",
                        next.generation, next.rollup, prev.generation, prev.rollup
                    ));
                }
            }
        }
        // The newest generation of a range must itself be live: superseded by nothing, or by a
        // block that is not in the range at all.
        if let Some(last) = group.last() {
            if let Some(by) = &last.superseded_by {
                if !group.iter().any(|o| &o.rollup == by) {
                    return Err(format!(
                        "block `{}` names `{by}` as its replacement, but no block covers tier \
                         {tier} range {from}..{to} of `{traj}` at generation {}",
                        last.rollup,
                        last.generation + 1
                    ));
                }
            }
        }
    }
    Ok(())
}

/// PURE: judge sealed blocks against the tiers-are-an-index statement.
///
/// `steps` and `rollups` are the ids that EXIST in the store. A block naming anything outside them
/// is a dangling index entry, which is exactly what §3 forbids.
pub fn evaluate_index(
    obs: &[Obs],
    steps: &BTreeSet<StepId>,
    rollups: &BTreeSet<RollupId>,
) -> Result<(), String> {
    for o in obs {
        if let Some(missing) = o.beneath_steps.iter().find(|s| !steps.contains(*s)) {
            return Err(format!(
                "sealed block `{}` names step `{missing}`, which is not in the store; tiers are \
                 an index, never a replacement",
                o.rollup
            ));
        }
        if let Some(missing) = o.beneath_rollups.iter().find(|r| !rollups.contains(*r)) {
            return Err(format!(
                "sealed block `{}` names rollup `{missing}`, which is not in the store; tiers are \
                 an index, never a replacement",
                o.rollup
            ));
        }
    }
    Ok(())
}

/// Both specs, for a provider's `Plugin::invariants()`.
pub fn specs() -> Vec<InvariantSpec> {
    vec![seal_once(), tiers_are_an_index()]
}

/// The plugin name both specs report under. It is the SEAM's name, not a provider's: the two
/// providers are judged by one statement, and a violation belongs to the statement.
pub const SEAM: &str = "rollups";

/// §3's seal-once, over the sealed rows themselves.
pub fn seal_once() -> InvariantSpec {
    InvariantSpec {
        name: "a_range_is_sealed_once_and_generations_never_skip",
        plugin: SEAM,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check_seal_once(ctx)),
    }
}

/// §3: tiers are an INDEX — every ref a sealed block names resolves.
pub fn tiers_are_an_index() -> InvariantSpec {
    InvariantSpec {
        name: "every_ref_a_sealed_block_names_resolves",
        plugin: SEAM,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check_index(ctx)),
    }
}

/// Every sealed block in the store, as observations. `None` when the store cannot be read — a
/// missing binding is the kernel's to report (the `plugins/ledger` precedent), and a read failure
/// is not a violation of either statement.
async fn blocks(ctx: &Context) -> Option<Vec<Obs>> {
    let ledger = ctx.try_get::<Ledger>().ok().flatten()?;
    sealed_blocks(&LedgerHandle(ledger.0.clone())).await.ok()
}

/// Every sealed tier block in a store, as observations. The reader BOTH checks use, exported so a
/// test can judge the same relation the runner judges rather than a parallel one.
pub async fn sealed_blocks(ledger: &LedgerHandle) -> Result<Vec<Obs>, LedgerError> {
    let rows = ledger
        .0
        .rollups(&RollupQuery {
            include_superseded: true,
            ..Default::default()
        })
        .await?;
    Ok(rows
        .into_iter()
        .filter(|r| r.kind == RollupKind::Tier)
        .map(|r| {
            let (steps, rollups) = match serde_json::from_value::<crate::TierBlock>(r.body) {
                Ok(b) => crate::block::refs_of(&b),
                Err(_) => (Vec::new(), Vec::new()),
            };
            Obs {
                generation: crate::plan::generation_of(&r.id).unwrap_or(0),
                rollup: r.id,
                traj: r.traj,
                tier: r.tier,
                from_seq: r.from_seq,
                to_seq: r.to_seq,
                superseded_by: r.superseded_by,
                beneath_steps: steps,
                beneath_rollups: rollups,
            }
        })
        .collect())
}

async fn check_seal_once(ctx: Context) -> Result<(), InvariantViolation> {
    let Some(obs) = blocks(&ctx).await else {
        return Ok(());
    };
    evaluate_seal_once(&obs).map_err(|detail| violation(&ctx, seal_once().name, detail))
}

async fn check_index(ctx: Context) -> Result<(), InvariantViolation> {
    let Some(obs) = blocks(&ctx).await else {
        return Ok(());
    };
    if obs.is_empty() {
        return Ok(());
    }
    let Ok(Some(ledger)) = ctx.try_get::<Ledger>() else {
        return Ok(());
    };
    let mut steps: BTreeSet<StepId> = BTreeSet::new();
    for id in obs.iter().flat_map(|o| o.beneath_steps.iter()) {
        if steps.contains(id) {
            continue;
        }
        match ledger.0.step(id).await {
            Ok(Some(s)) => {
                steps.insert(s.id);
            }
            Ok(None) => {}
            // A read failure is not a violation of THIS statement.
            Err(_) => return Ok(()),
        }
    }
    let Ok(rows) = ledger
        .0
        .rollups(&RollupQuery {
            include_superseded: true,
            ..Default::default()
        })
        .await
    else {
        return Ok(());
    };
    let rollups: BTreeSet<RollupId> = rows.into_iter().map(|r| r.id).collect();
    evaluate_index(&obs, &steps, &rollups)
        .map_err(|detail| violation(&ctx, tiers_are_an_index().name, detail))
}

fn violation(ctx: &Context, invariant: &'static str, detail: String) -> InvariantViolation {
    InvariantViolation {
        invariant,
        plugin: SEAM,
        entry: ctx.entry_id().clone(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(tier: u8, from: u64, to: u64, generation: u32) -> Obs {
        Obs {
            rollup: crate::plan::tier_id(&TrajId::new("t"), tier, Seq(from), Seq(to), generation),
            traj: TrajId::new("t"),
            tier,
            from_seq: Seq(from),
            to_seq: Seq(to),
            generation,
            superseded_by: None,
            beneath_steps: vec![StepId::new("s1")],
            beneath_rollups: vec![],
        }
    }

    /// A generation-n block replaced by n+1, linked the way the store holds it.
    fn superseded(tier: u8, from: u64, to: u64, generation: u32) -> Obs {
        let next =
            crate::plan::tier_id(&TrajId::new("t"), tier, Seq(from), Seq(to), generation + 1);
        Obs {
            superseded_by: Some(next),
            ..obs(tier, from, to, generation)
        }
    }

    #[test]
    fn a_planted_second_live_block_over_one_range_is_reported() {
        // Generation 1 written without linking generation 0: two live blocks over one range.
        let err = evaluate_seal_once(&[obs(1, 1, 4, 0), obs(1, 1, 4, 1)])
            .expect_err("an unlinked replacement is a seal-once violation");
        assert!(err.contains("still live"), "{err}");
        assert!(err.contains("1..4"), "the report names the range: {err}");
    }

    #[test]
    fn a_generation_that_skips_a_number_is_reported() {
        let mut zero = superseded(1, 1, 4, 0);
        zero.superseded_by = Some(crate::plan::tier_id(
            &TrajId::new("t"),
            1,
            Seq(1),
            Seq(4),
            2,
        ));
        let err = evaluate_seal_once(&[zero, obs(1, 1, 4, 2)])
            .expect_err("a supersession is generation n+1, never n+2");
        assert!(err.contains("jumps from generation 0 to 2"), "{err}");
    }

    #[test]
    fn a_replacement_that_names_the_wrong_block_is_reported() {
        let mut zero = superseded(1, 1, 4, 0);
        zero.superseded_by = Some(RollupId::new("tier:t:1:1-9"));
        let err = evaluate_seal_once(&[zero, obs(1, 1, 4, 1)])
            .expect_err("a mislinked supersession is a violation");
        assert!(err.contains("`superseded_by` names"), "{err}");
    }

    #[test]
    fn a_dangling_replacement_link_is_reported() {
        let err = evaluate_seal_once(&[superseded(1, 1, 4, 0)])
            .expect_err("a link to a block that was never written is a violation");
        assert!(err.contains("names"), "{err}");
        assert!(err.contains("generation 1"), "{err}");
    }

    #[test]
    fn a_block_naming_a_missing_step_is_reported() {
        let stream = [obs(1, 1, 4, 0)];
        let err = evaluate_index(&stream, &BTreeSet::new(), &BTreeSet::new())
            .expect_err("a dangling index entry is a violation");
        assert!(err.contains("names step `s1`"), "{err}");
        // The same blocks against a store that HAS the step passes.
        let steps = BTreeSet::from([StepId::new("s1")]);
        assert!(evaluate_index(&stream, &steps, &BTreeSet::new()).is_ok());
    }

    #[test]
    fn a_clean_store_passes() {
        let stream = [
            obs(1, 5, 8, 0),
            // The relief valve: generation 0 superseded by a linked generation 1.
            superseded(1, 1, 4, 0),
            obs(1, 1, 4, 1),
            Obs {
                beneath_rollups: vec![crate::plan::tier_id(
                    &TrajId::new("t"),
                    1,
                    Seq(1),
                    Seq(4),
                    0,
                )],
                ..obs(2, 1, 8, 0)
            },
        ];
        assert!(evaluate_seal_once(&stream).is_ok());
        assert!(evaluate_seal_once(&[]).is_ok());
        let steps = BTreeSet::from([StepId::new("s1")]);
        let rollups = BTreeSet::from([crate::plan::tier_id(
            &TrajId::new("t"),
            1,
            Seq(1),
            Seq(4),
            0,
        )]);
        assert!(evaluate_index(&stream, &steps, &rollups).is_ok());
    }
}
