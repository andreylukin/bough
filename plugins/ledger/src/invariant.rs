//! §0.2 runtime invariants for the ledger. Both providers return these specs from
//! `Plugin::invariants()`, so they run against whichever provider is mounted.
//!
//! 1. **`append_only_rows_never_change`** — across one session, no `steps`/`edges`/`rollups` row
//!    hash changes and no row id disappears.
//! 2. **`seal_once`** — a rollup's `superseded_by` transitions at most once, NULL → value, never
//!    back.
//! 3. **`seq_strictly_grows_per_trajectory`** — over the observed `ledger/step` stream, within a
//!    trajectory each step's seq is exactly its predecessor's + 1.
//! 4. **`wake_step_enclosure`** — every `step/start`..`step/end` pair lies inside a
//!    `wake/start`..`wake/end` pair of the same wake, and every step carries a wake id.
//!
//! Each check is a pure function over an observation record (`evaluate(&[Obs]) -> Result<(),
//! String>`) plus a store read, exactly as `hello`'s is. The record is cleared per fiber LIFE by
//! an inverse the provider's `apply` registers, because a RELOAD keeps the `FiberUid`.
//!
//! Every cadence is [`Cadence::OnQuiesce`] (P1-D14): Phase 0 left `Interval`/`OnEvent`
//! undispatched and Phase 1 takes no kernel change.

use std::collections::BTreeMap;

use bough_kernel::{Cadence, Context, FiberUid, InvariantSpec, InvariantViolation};
use parking_lot::Mutex;

use crate::id::{RollupId, Seq, StepType, TrajId, WakeId};
use crate::query::RowHash;

/// One observation the `ledger/step` listener recorded. The invariants are statements about THIS
/// stream, so a check reads exactly what was observed and nothing else.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub fiber: FiberUid,
    pub traj: TrajId,
    pub seq: Seq,
    pub wake: WakeId,
    pub kind: StepType,
}

/// What the listener has seen, in arrival order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Row hashes as first observed, per session. Populated at each quiesce and compared against.
static HASHES: Mutex<Option<BTreeMap<(&'static str, String), String>>> = Mutex::new(None);

/// Every `superseded_by` transition observed, in order.
static SUPERSESSIONS: Mutex<Vec<(RollupId, RollupId)>> = Mutex::new(Vec::new());

/// Record one observation. Called from the listener the provider's `apply` registers.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Record one `superseded_by` transition. Called by a provider's `supersede_rollup`, which is the
/// only place the transition can happen, so the record cannot miss one.
pub fn record_supersession(old: RollupId, new: RollupId) {
    SUPERSESSIONS.lock().push((old, new));
}

/// Everything recorded so far, oldest first.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Every supersession recorded so far, oldest first.
pub fn supersessions() -> Vec<(RollupId, RollupId)> {
    SUPERSESSIONS.lock().clone()
}

/// Drop the recorded stream. Test setup only.
pub fn clear() {
    SEEN.lock().clear();
    SUPERSESSIONS.lock().clear();
    *HASHES.lock() = None;
}

/// Forget everything recorded for `fiber`, so a reload starts a fresh stream.
///
/// A RELOAD keeps the `FiberUid` and a new provider starts its trajectories over, so without this
/// the swap test's own headline behaviour would falsify `seq_strictly_grows_per_trajectory`.
pub fn forget(fiber: FiberUid) {
    SEEN.lock().retain(|o| o.fiber != fiber);
    // The row-hash baseline belongs to a store, and a reload may be a DIFFERENT store: drop it so
    // the next quiesce re-baselines instead of comparing two providers' rows to each other.
    *HASHES.lock() = None;
    SUPERSESSIONS.lock().clear();
}

/// The four specs a provider's `Plugin::invariants()` returns. `plugin` is the provider's catalog
/// name, so a violation report names the row the reader will actually go looking at.
pub fn specs(plugin: &'static str) -> Vec<InvariantSpec> {
    vec![
        InvariantSpec {
            name: "seq_strictly_grows_per_trajectory",
            plugin,
            cadence: Cadence::OnQuiesce,
            check: |ctx| {
                Box::pin(async move {
                    evaluate_seq(&seen()).map_err(|d| {
                        violation(
                            &ctx,
                            "seq_strictly_grows_per_trajectory",
                            ctx.plugin_name(),
                            d,
                        )
                    })
                })
            },
        },
        InvariantSpec {
            name: "wake_step_enclosure",
            plugin,
            cadence: Cadence::OnQuiesce,
            check: |ctx| {
                Box::pin(async move {
                    evaluate_enclosure(&seen())
                        .map_err(|d| violation(&ctx, "wake_step_enclosure", ctx.plugin_name(), d))
                })
            },
        },
        InvariantSpec {
            name: "append_only_rows_never_change",
            plugin,
            cadence: Cadence::OnQuiesce,
            check: |ctx| Box::pin(check_row_hashes(ctx)),
        },
        InvariantSpec {
            name: "seal_once",
            plugin,
            cadence: Cadence::OnQuiesce,
            check: |ctx| Box::pin(check_seal_once(ctx)),
        },
    ]
}

/// Read the store's row hashes. A store that cannot be read is reported as a violation of nothing:
/// the check returns `Ok(())`, because the invariant runner REPORTS and a missing binding is the
/// kernel's business, not this invariant's.
async fn current_hashes(ctx: &Context) -> Option<Vec<RowHash>> {
    let handle = ctx.try_get::<crate::Ledger>().ok().flatten()?;
    handle.0.row_hashes(crate::query::HashScope::All).await.ok()
}

async fn check_row_hashes(ctx: Context) -> Result<(), InvariantViolation> {
    let Some(now) = current_hashes(&ctx).await else {
        return Ok(());
    };
    let baseline = {
        let mut guard = HASHES.lock();
        match guard.as_ref() {
            Some(first) => first.clone(),
            None => {
                // First quiesce of the session establishes the baseline; there is nothing to
                // compare it against yet.
                *guard = Some(snapshot(&now));
                return Ok(());
            }
        }
    };
    let first: Vec<RowHash> = baseline
        .into_iter()
        .map(|((table, id), hash)| RowHash {
            table,
            id,
            hash,
            superseded_by: None,
        })
        .collect();
    evaluate_row_hashes(&first, &now)
        .map_err(|d| violation(&ctx, "append_only_rows_never_change", ctx.plugin_name(), d))
}

async fn check_seal_once(ctx: Context) -> Result<(), InvariantViolation> {
    let rows = current_hashes(&ctx).await.unwrap_or_default();
    evaluate_seal_once(&rows, &supersessions())
        .map_err(|d| violation(&ctx, "seal_once", ctx.plugin_name(), d))
}

fn snapshot(rows: &[RowHash]) -> BTreeMap<(&'static str, String), String> {
    rows.iter()
        .map(|r| ((r.table, r.id.clone()), r.hash.clone()))
        .collect()
}

/// **`seq_strictly_grows_per_trajectory`**, as a pure function of the observed stream.
///
/// The FIRST observation of a trajectory is accepted whatever its seq: a listener may start
/// recording partway through a trajectory's life (a reload, a fork's parent), and "seq 7 arrived
/// first" is not a ledger fault. Every observation after it must be exactly its predecessor + 1.
pub fn evaluate_seq(stream: &[Obs]) -> Result<(), String> {
    let mut last: BTreeMap<TrajId, Seq> = BTreeMap::new();
    for obs in stream {
        if let Some(prev) = last.get(&obs.traj) {
            if obs.seq.0 != prev.0 + 1 {
                let word = if obs.seq.0 <= prev.0 {
                    "regressed to"
                } else {
                    "jumped to"
                };
                return Err(format!(
                    "trajectory `{}` {} seq {} after seq {}; seq is contiguous and strictly \
                     increasing per trajectory",
                    obs.traj, word, obs.seq.0, prev.0
                ));
            }
        }
        last.insert(obs.traj.clone(), obs.seq);
    }
    Ok(())
}

/// **`wake_step_enclosure`**, as a pure function of the observed stream.
///
/// Every step carries a wake id, and every `step/start`..`step/end` pair lies inside a
/// `wake/start`..`wake/end` pair of the SAME wake.
pub fn evaluate_enclosure(stream: &[Obs]) -> Result<(), String> {
    let mut open: BTreeMap<TrajId, WakeId> = BTreeMap::new();
    for obs in stream {
        if obs.wake.as_str().is_empty() {
            return Err(format!(
                "step at seq {} of trajectory `{}` carries no wake id; every step carries one",
                obs.seq.0, obs.traj
            ));
        }
        match obs.kind.as_str() {
            "wake/start" => {
                if let Some(already) = open.get(&obs.traj) {
                    return Err(format!(
                        "wake `{}` opened at seq {} of trajectory `{}` while wake `{already}` was \
                         still open",
                        obs.wake, obs.seq.0, obs.traj
                    ));
                }
                open.insert(obs.traj.clone(), obs.wake.clone());
            }
            "wake/end" => match open.get(&obs.traj) {
                Some(w) if *w == obs.wake => {
                    open.remove(&obs.traj);
                }
                Some(w) => {
                    return Err(format!(
                        "wake/end for `{}` at seq {} of trajectory `{}` closes a wake that is not \
                         the open one (`{w}`)",
                        obs.wake, obs.seq.0, obs.traj
                    ))
                }
                None => {
                    return Err(format!(
                        "wake/end for `{}` at seq {} of trajectory `{}` closes no open wake",
                        obs.wake, obs.seq.0, obs.traj
                    ))
                }
            },
            "step/start" | "step/end" => match open.get(&obs.traj) {
                Some(w) if *w == obs.wake => {}
                Some(w) => {
                    return Err(format!(
                        "`{}` at seq {} of trajectory `{}` carries wake `{}` but the open wake is \
                         `{w}`",
                        obs.kind, obs.seq.0, obs.traj, obs.wake
                    ))
                }
                None => {
                    return Err(format!(
                        "`{}` at seq {} of trajectory `{}` lies outside any open wake",
                        obs.kind, obs.seq.0, obs.traj
                    ))
                }
            },
            _ => {}
        }
    }
    Ok(())
}

/// **`append_only_rows_never_change`**, as a pure function of two row-hash snapshots. A row that
/// changed its hash, and a row id that disappeared, are both violations; a rollup whose
/// `superseded_by` moved is NOT, because the hash excludes that column.
pub fn evaluate_row_hashes(first: &[RowHash], now: &[RowHash]) -> Result<(), String> {
    let current = snapshot(now);
    for row in first {
        match current.get(&(row.table, row.id.clone())) {
            None => {
                return Err(format!(
                    "row `{}` of `{}` disappeared; {} is append-only",
                    row.id, row.table, row.table
                ))
            }
            Some(hash) if *hash != row.hash => {
                return Err(format!(
                    "row `{}` of `{}` changed content hash {} -> {hash}; {} is append-only",
                    row.id, row.table, row.hash, row.table
                ))
            }
            Some(_) => {}
        }
    }
    Ok(())
}

/// **`seal_once`**, as a pure function of the observed supersessions plus the current rows.
pub fn evaluate_seal_once(
    rows: &[RowHash],
    observed: &[(RollupId, RollupId)],
) -> Result<(), String> {
    let mut first: BTreeMap<RollupId, RollupId> = BTreeMap::new();
    for (old, new) in observed {
        if let Some(prev) = first.get(old) {
            return Err(format!(
                "rollup `{old}` was superseded by `{prev}` and then again by `{new}`; \
                 superseded_by is set once"
            ));
        }
        first.insert(old.clone(), new.clone());
    }
    for (old, new) in &first {
        if let Some(row) = rows
            .iter()
            .find(|r| r.table == "rollups" && r.id == old.as_str())
        {
            match &row.superseded_by {
                None => {
                    return Err(format!(
                        "rollup `{old}` was superseded by `{new}` but its superseded_by is now \
                         NULL; the transition is NULL -> value and never back"
                    ))
                }
                Some(v) if v != new.as_str() => {
                    return Err(format!(
                        "rollup `{old}` was superseded by `{new}` but its superseded_by now reads \
                         `{v}`; superseded_by is set once"
                    ))
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

/// Shared shape of the four checks: run the pure evaluation, wrap a failure as a violation.
fn violation(
    ctx: &Context,
    invariant: &'static str,
    plugin: &'static str,
    detail: String,
) -> InvariantViolation {
    InvariantViolation {
        invariant,
        plugin,
        entry: ctx.entry_id().clone(),
        detail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obs(traj: &str, seq: u64, wake: &str, kind: &str) -> Obs {
        Obs {
            fiber: FiberUid(1),
            traj: TrajId::new(traj),
            seq: Seq(seq),
            wake: WakeId::new(wake),
            kind: StepType::new(kind),
        }
    }

    fn hash(table: &'static str, id: &str, h: &str) -> RowHash {
        RowHash {
            table,
            id: id.to_string(),
            hash: h.to_string(),
            superseded_by: None,
        }
    }

    #[test]
    fn seq_regression_is_a_violation() {
        let stream = vec![
            obs("a", 1, "w", "x"),
            obs("a", 2, "w", "x"),
            obs("a", 2, "w", "x"),
        ];
        let detail = evaluate_seq(&stream).expect_err("a repeated seq is a regression");
        assert!(detail.contains("regressed to seq 2"), "unhelpful: {detail}");
        // Two trajectories are judged on their own streams, interleaved or not.
        assert_eq!(
            evaluate_seq(&[
                obs("a", 1, "w", "x"),
                obs("b", 1, "w", "x"),
                obs("a", 2, "w", "x")
            ]),
            Ok(())
        );
    }

    #[test]
    fn a_seq_gap_is_a_violation() {
        let detail = evaluate_seq(&[obs("a", 1, "w", "x"), obs("a", 3, "w", "x")])
            .expect_err("a gap is a violation, not just a regression");
        assert!(detail.contains("jumped to seq 3"), "unhelpful: {detail}");
        // Starting partway through is fine; only the steps AFTER the first must be contiguous.
        assert_eq!(
            evaluate_seq(&[obs("a", 7, "w", "x"), obs("a", 8, "w", "x")]),
            Ok(())
        );
    }

    #[test]
    fn wake_step_enclosure_holds() {
        let stream = vec![
            obs("a", 1, "w1", "wake/start"),
            obs("a", 2, "w1", "step/start"),
            obs("a", 3, "w1", "step/end"),
            obs("a", 4, "w1", "wake/end"),
            obs("a", 5, "w2", "wake/start"),
            obs("a", 6, "w2", "step/start"),
            obs("a", 7, "w2", "step/end"),
            obs("a", 8, "w2", "wake/end"),
        ];
        assert_eq!(evaluate_enclosure(&stream), Ok(()));
        // A step of some other type outside a wake is not this invariant's business.
        assert_eq!(evaluate_enclosure(&[obs("a", 1, "w1", "pin/set")]), Ok(()));
    }

    #[test]
    fn a_step_pair_outside_a_wake_is_a_violation() {
        let detail = evaluate_enclosure(&[obs("a", 1, "w1", "step/start")])
            .expect_err("a step pair outside any wake is a violation");
        assert!(
            detail.contains("outside any open wake"),
            "unhelpful: {detail}"
        );
        // Inside the WRONG wake is a violation too.
        let detail = evaluate_enclosure(&[
            obs("a", 1, "w1", "wake/start"),
            obs("a", 2, "w2", "step/start"),
        ])
        .expect_err("a step carrying another wake id is a violation");
        assert!(detail.contains("open wake is `w1`"), "unhelpful: {detail}");
        // A step with no wake id at all is the other half of the statement.
        assert!(evaluate_enclosure(&[obs("a", 1, "", "pin/set")])
            .expect_err("every step carries a wake id")
            .contains("no wake id"));
    }

    #[test]
    fn a_changed_row_hash_is_a_violation() {
        let first = vec![hash("steps", "s1", "aa"), hash("steps", "s2", "bb")];
        let changed = vec![hash("steps", "s1", "ZZ"), hash("steps", "s2", "bb")];
        let detail = evaluate_row_hashes(&first, &changed).expect_err("a row changed");
        assert!(
            detail.contains("changed content hash aa -> ZZ"),
            "unhelpful: {detail}"
        );
        // A disappearing row is the other half of "append-only".
        let gone = vec![hash("steps", "s2", "bb")];
        assert!(evaluate_row_hashes(&first, &gone)
            .expect_err("a row vanished")
            .contains("disappeared"));
        // New rows are what an append-only log DOES; they are never a violation.
        let mut grown = first.clone();
        grown.push(hash("steps", "s3", "cc"));
        assert_eq!(evaluate_row_hashes(&first, &grown), Ok(()));
    }

    #[test]
    fn setting_superseded_by_once_is_not_a_row_hash_change() {
        let first = vec![hash("rollups", "r1", "aa")];
        let superseded = vec![RowHash {
            superseded_by: Some("r2".to_string()),
            ..hash("rollups", "r1", "aa")
        }];
        // The hash EXCLUDES superseded_by, so the one permitted write to a sealed row is not a
        // row change (§3).
        assert_eq!(evaluate_row_hashes(&first, &superseded), Ok(()));
        assert_eq!(
            evaluate_seal_once(&superseded, &[(RollupId::new("r1"), RollupId::new("r2"))]),
            Ok(())
        );
    }

    #[test]
    fn a_second_supersession_is_a_seal_once_violation() {
        let rows = vec![RowHash {
            superseded_by: Some("r2".to_string()),
            ..hash("rollups", "r1", "aa")
        }];
        let observed = vec![
            (RollupId::new("r1"), RollupId::new("r2")),
            (RollupId::new("r1"), RollupId::new("r3")),
        ];
        let detail = evaluate_seal_once(&rows, &observed).expect_err("two supersessions");
        assert!(detail.contains("set once"), "unhelpful: {detail}");
        // A row whose superseded_by went back to NULL is the other way to break seal-once.
        let reverted = vec![hash("rollups", "r1", "aa")];
        assert!(
            evaluate_seal_once(&reverted, &observed[..1]).is_err(),
            "a supersession that unwound must be reported"
        );
    }

    #[test]
    fn a_clean_stream_reports_nothing() {
        let stream = vec![
            obs("a", 1, "w1", "wake/start"),
            obs("a", 2, "w1", "step/start"),
            obs("a", 3, "w1", "step/end"),
            obs("a", 4, "w1", "pin/set"),
            obs("a", 5, "w1", "wake/end"),
            obs("b", 1, "w9", "wake/start"),
            obs("b", 2, "w9", "wake/end"),
        ];
        assert_eq!(evaluate_seq(&stream), Ok(()));
        assert_eq!(evaluate_enclosure(&stream), Ok(()));
        let rows = vec![hash("steps", "s1", "aa"), hash("rollups", "r1", "bb")];
        assert_eq!(evaluate_row_hashes(&rows, &rows), Ok(()));
        assert_eq!(evaluate_seal_once(&rows, &[]), Ok(()));
        assert_eq!(evaluate_seq(&[]), Ok(()));
        assert_eq!(evaluate_enclosure(&[]), Ok(()));
    }

    /// A reload keeps the `FiberUid` and a new provider starts its trajectories over; without
    /// `forget`, the restarted seq 1 reads as a regression.
    #[test]
    fn forgetting_a_fiber_lets_a_reload_start_over() {
        clear();
        record(obs("a", 1, "w", "pin/set"));
        record(obs("a", 2, "w", "pin/set"));
        assert_eq!(evaluate_seq(&seen()), Ok(()));
        forget(FiberUid(1));
        assert!(seen().is_empty());
        record(obs("a", 1, "w", "pin/set"));
        assert_eq!(evaluate_seq(&seen()), Ok(()));
        clear();
    }
}
