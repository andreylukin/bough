//! §0.2 runtime invariants for the rollups seam. Both are returned by the PROVIDERS'
//! `Plugin::invariants()`; the Definition owns the statement so two providers cannot disagree
//! about what "sealed once" means.
//!
//! 1. **`seal_once`** — over the observed `ledger/step` stream filtered to `rollup/sealed`: no two
//!    observations name the same `(traj, tier, from_seq, to_seq, gen)`, and no observation names a
//!    `(traj, tier, from_seq, to_seq)` whose generation is not exactly one above the highest
//!    already seen for it. This is the event-stream half V1 asks for; the ledger's own
//!    `seal_once` (a `superseded_by` transition happens at most once) is the row half.
//! 2. **`tiers_are_an_index`** — for every `rollup/sealed` observed, every id in the block's
//!    `beneath` and `evidence` resolves to a row that exists in the store at quiesce.
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] for both (P1-D14; the kernel dispatches no
//! other).

use std::collections::{BTreeMap, BTreeSet};

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{Ledger, RollupId, RollupQuery, Seq, StepId, TrajId};
use parking_lot::Mutex;

/// One sealed block, as observed on the `ledger/step` stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub rollup: RollupId,
    pub traj: TrajId,
    pub tier: u8,
    pub from_seq: Seq,
    pub to_seq: Seq,
    /// The generation encoded in the block's deterministic id (0 for the original).
    pub generation: u32,
    /// Ids the block's `beneath` and `evidence` name; `tiers_are_an_index` resolves them.
    pub beneath_steps: Vec<StepId>,
    pub beneath_rollups: Vec<RollupId>,
}

/// What the providers recorded this session, in seal order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one sealed block. Called by a provider after `rollup/sealed` is appended.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Everything recorded this session.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Forget the record. Tests only; the runner never calls it.
pub fn reset() {
    SEEN.lock().clear();
}

/// PURE: judge a stream of observations against the seal-once statement.
///
/// Written as a function of data so a planted violation is a unit test rather than a live run.
pub fn evaluate_seal_once(obs: &[Obs]) -> Result<(), String> {
    // The FIRST observation of a range is accepted whatever its generation: a listener may start
    // recording partway through a trajectory's life (a reload, an adopted store), and "generation
    // 3 arrived first" is not a seal-once fault. Every observation after it must be exactly one
    // above the highest seen — which makes both a re-seal (same generation) and a skipped
    // generation a violation, with no third rule.
    let mut highest: BTreeMap<(TrajId, u8, u64, u64), u32> = BTreeMap::new();
    for o in obs {
        let key = (o.traj.clone(), o.tier, o.from_seq.0, o.to_seq.0);
        match highest.get(&key) {
            None => {
                highest.insert(key, o.generation);
            }
            Some(&prev) if o.generation == prev + 1 => {
                highest.insert(key, o.generation);
            }
            Some(&prev) => {
                let what = if o.generation <= prev {
                    format!(
                        "was sealed again at generation {} after generation {prev}",
                        o.generation
                    )
                } else {
                    format!(
                        "jumped to generation {} after generation {prev}",
                        o.generation
                    )
                };
                return Err(format!(
                    "tier {} range {}..{} of `{}` {what} (block `{}`); a range is summarized \
                     exactly once and a supersession is generation n+1",
                    o.tier, o.from_seq.0, o.to_seq.0, o.traj, o.rollup
                ));
            }
        }
    }
    Ok(())
}

/// PURE: judge a stream of observations against the tiers-are-an-index statement.
///
/// `steps` and `rollups` are the ids that EXIST in the store. An observation naming anything
/// outside them is a dangling index entry, which is exactly what §3 forbids.
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

/// The event-stream half of §3's seal-once.
pub fn seal_once() -> InvariantSpec {
    InvariantSpec {
        name: "a_range_is_sealed_once_and_generations_never_skip",
        plugin: "rollups",
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check_seal_once(ctx)),
    }
}

/// §3: tiers are an INDEX — every ref a sealed block names resolves.
pub fn tiers_are_an_index() -> InvariantSpec {
    InvariantSpec {
        name: "every_ref_a_sealed_block_names_resolves",
        plugin: "rollups",
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check_index(ctx)),
    }
}

async fn check_seal_once(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate_seal_once(&seen()).map_err(|detail| violation(&ctx, seal_once().name, detail))
}

async fn check_index(ctx: Context) -> Result<(), InvariantViolation> {
    let obs = seen();
    if obs.is_empty() {
        return Ok(());
    }
    // A store that cannot be read is not this invariant's business: a missing binding is the
    // kernel's to report (the `plugins/ledger` precedent).
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
    let trajs: Vec<TrajId> = obs
        .iter()
        .map(|o| o.traj.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let Ok(rows) = ledger
        .0
        .rollups(&RollupQuery {
            trajs,
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
        plugin: "rollups",
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
            beneath_steps: vec![StepId::new("s1")],
            beneath_rollups: vec![],
        }
    }

    #[test]
    fn a_planted_reseal_of_the_same_range_is_reported() {
        let err = evaluate_seal_once(&[obs(1, 1, 4, 0), obs(1, 1, 4, 0)])
            .expect_err("the same range sealed twice is the seal-once violation");
        assert!(err.contains("sealed again"), "{err}");
        assert!(err.contains("1..4"), "the report names the range: {err}");
    }

    #[test]
    fn a_generation_that_skips_a_number_is_reported() {
        let err = evaluate_seal_once(&[obs(1, 1, 4, 0), obs(1, 1, 4, 2)])
            .expect_err("a supersession is generation n+1, never n+2");
        assert!(err.contains("jumped to generation 2"), "{err}");
    }

    #[test]
    fn a_block_naming_a_missing_step_is_reported() {
        let stream = [obs(1, 1, 4, 0)];
        let err = evaluate_index(&stream, &BTreeSet::new(), &BTreeSet::new())
            .expect_err("a dangling index entry is a violation");
        assert!(err.contains("names step `s1`"), "{err}");
        // The same stream against a store that HAS the step passes.
        let steps = BTreeSet::from([StepId::new("s1")]);
        assert!(evaluate_index(&stream, &steps, &BTreeSet::new()).is_ok());
    }

    #[test]
    fn a_clean_stream_passes() {
        let stream = [
            obs(1, 1, 4, 0),
            obs(1, 5, 8, 0),
            // The relief valve: the same range at generation 1.
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
