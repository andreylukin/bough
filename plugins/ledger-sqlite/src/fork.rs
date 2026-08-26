//! Invariant: a fork's prefix must END OUTSIDE an open wake (§3). A prefix that lands inside one
//! is REFUSED, naming the wake and the seq it opened at — never silently clipped — and a refused
//! fork writes nothing at all. A successful fork writes the edge and the child's `fork/end-seed`
//! marker at seq 1 in ONE transaction.

use std::sync::Arc;

use bough_plugin_ledger::vocabulary::ForkEndSeed;
use bough_plugin_ledger::{
    Append, Class, Edge, EdgeKind, Fork, ForkOutcome, LedgerError, LedgerStep, Seq, StepType,
    WakeId,
};

use crate::schema::store_err;
use crate::store::SqliteStore;

/// The whole fork path.
pub async fn fork(store: &SqliteStore, req: Fork) -> Result<ForkOutcome, LedgerError> {
    // The end-seed is a normal append and is validated like one, before anything is written.
    let seed = crate::append::prepare(
        store,
        Append {
            traj: req.child.clone(),
            wake: WakeId::seed(&req.child),
            kind: StepType::new("fork/end-seed"),
            class: Class::Thought,
            body: serde_json::to_value(ForkEndSeed {
                parent: req.parent.clone(),
                at_seq: req.at_seq,
            })
            .map_err(crate::append::json_err)?,
            cites: vec![],
            at: req.at,
            id: None,
        },
    )?;

    let edge = Edge {
        child: req.child.clone(),
        parent: req.parent.clone(),
        at_seq: req.at_seq,
        kind: EdgeKind::Ancestor,
        at: req.at,
    };
    let outcome_edge = edge.clone();
    let req_for_scan = req.clone();

    let end_seed = store
        .with_conn(move |conn| {
            let tx = conn.transaction().map_err(store_err)?;

            // The parent must exist as a trajectory before it can be forked.
            let head: Option<i64> = tx
                .query_row(
                    "SELECT MAX(seq) FROM steps WHERE traj_id = ?1",
                    rusqlite::params![req_for_scan.parent.as_str()],
                    |r| r.get(0),
                )
                .map_err(store_err)?;
            let Some(head) = head else {
                return Err(LedgerError::NoSuchTrajectory(req_for_scan.parent.clone()));
            };
            if (head as u64) < req_for_scan.at_seq.0 {
                return Err(LedgerError::Store(anyhow::anyhow!(
                    "fork of `{}` at seq {} is past its head seq {head}",
                    req_for_scan.parent,
                    req_for_scan.at_seq.0
                )));
            }

            let markers = wake_markers(&tx, &req_for_scan)?;
            if let Some((wake, opened_at)) = open_wake_at(&markers, req_for_scan.at_seq) {
                // Refused BEFORE any write: the transaction rolls back with nothing in it.
                return Err(LedgerError::ForkInsideOpenWake {
                    parent: req_for_scan.parent.clone(),
                    at_seq: req_for_scan.at_seq,
                    wake,
                    opened_at,
                });
            }

            crate::read::insert_edge(&tx, &edge)?;
            let step = crate::append::insert_step(&tx, &seed)?;
            tx.commit().map_err(store_err)?;
            Ok(step)
        })
        .await?;

    // The seed is a step, so it broadcasts `ledger/step` like any other — post-commit.
    store.ctx.emit::<LedgerStep>(Arc::new(end_seed.clone()));
    Ok(ForkOutcome {
        edge: outcome_edge,
        end_seed,
    })
}

/// The parent's `wake/start` / `wake/end` markers up to and including `at_seq`, oldest first.
fn wake_markers(
    tx: &rusqlite::Transaction<'_>,
    req: &Fork,
) -> Result<Vec<(Seq, WakeId, bool)>, LedgerError> {
    let mut stmt = tx
        .prepare(
            "SELECT seq, wake_id, type FROM steps \
             WHERE traj_id = ?1 AND seq <= ?2 AND type IN ('wake/start','wake/end') \
             ORDER BY seq ASC",
        )
        .map_err(store_err)?;
    let rows = stmt
        .query_map(
            rusqlite::params![req.parent.as_str(), req.at_seq.0 as i64],
            |r| {
                Ok((
                    Seq(r.get::<_, i64>(0)? as u64),
                    WakeId::new(r.get::<_, String>(1)?),
                    r.get::<_, String>(2)? == "wake/start",
                ))
            },
        )
        .map_err(store_err)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(store_err)?;
    Ok(rows)
}

/// Scan the parent's `wake/*` markers up to and including `at_seq`; `Some` names the wake still
/// open there. A pure function of the scanned markers, so the rule is testable without a store.
pub fn open_wake_at(markers: &[(Seq, WakeId, bool)], at_seq: Seq) -> Option<(WakeId, Seq)> {
    let mut open: Vec<(WakeId, Seq)> = Vec::new();
    for (seq, wake, is_start) in markers {
        if *seq > at_seq {
            break;
        }
        if *is_start {
            open.push((wake.clone(), *seq));
        } else {
            open.retain(|(w, _)| w != wake);
        }
    }
    // The oldest still-open wake is the one named: it is the one the prefix would cut into.
    open.into_iter().min_by_key(|(_, seq)| seq.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(seq: u64, wake: &str, start: bool) -> (Seq, WakeId, bool) {
        (Seq(seq), WakeId::new(wake), start)
    }

    #[test]
    fn a_closed_prefix_has_no_open_wake() {
        let markers = vec![m(1, "a", true), m(4, "a", false)];
        assert_eq!(open_wake_at(&markers, Seq(4)), None);
        assert_eq!(open_wake_at(&markers, Seq(9)), None);
    }

    #[test]
    fn a_prefix_inside_a_wake_names_it() {
        let markers = vec![m(1, "a", true), m(4, "a", false), m(5, "b", true)];
        assert_eq!(
            open_wake_at(&markers, Seq(6)),
            Some((WakeId::new("b"), Seq(5)))
        );
        // Cutting exactly at the wake/start is still inside it.
        assert_eq!(
            open_wake_at(&markers, Seq(5)),
            Some((WakeId::new("b"), Seq(5)))
        );
    }

    /// Concurrent wakes interleave, so "the last marker" is not the answer: the oldest wake still
    /// open at the cut is.
    #[test]
    fn interleaved_wakes_are_tracked_per_wake_id() {
        let markers = vec![
            m(1, "a", true),
            m(2, "b", true),
            m(3, "b", false),
            m(4, "c", true),
        ];
        assert_eq!(
            open_wake_at(&markers, Seq(4)),
            Some((WakeId::new("a"), Seq(1)))
        );
        assert_eq!(
            open_wake_at(&markers, Seq(3)).map(|(w, _)| w),
            Some(WakeId::new("a"))
        );
    }

    #[test]
    fn markers_after_the_cut_are_ignored() {
        let markers = vec![m(1, "a", true), m(2, "a", false), m(9, "z", true)];
        assert_eq!(open_wake_at(&markers, Seq(5)), None);
    }
}
