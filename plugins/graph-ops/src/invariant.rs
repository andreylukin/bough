//! §0.2 runtime invariants for the graph seam. All three read the LEDGER and the `agents` rows —
//! the authoritative relations — rather than what an op reported about itself.
//!
//! 1. **`split_has_two_edges`** — every `graph/split` step is preceded by exactly two ancestor
//!    edges and two `fork/end-seed` markers naming its `at_seq`.
//! 2. **`merge_is_reconciled`** — every `graph/merge` step is preceded by exactly two `merge`
//!    edges and one reconciliation rollup whose `src_trajs` are both parents.
//! 3. **`absorbed_row_is_gone`** — no `agents` row named by a `graph/merge` as `absorbed` exists
//!    afterwards. Both TRAJECTORIES remain: §3 deletes the losing row, never its past.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] for all three (P1-D14).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{
    AgentName, EdgeKind, Ledger, Order, RollupKind, RollupQuery, Seq, StepId, StepQuery, StepType,
    TrajId,
};

use crate::vocabulary::{GraphMerge, GraphSplit, GRAPH_MERGE, GRAPH_SPLIT};

/// One `graph/split`, with the relations that must stand around it.
#[derive(Clone, Debug)]
pub struct SplitObs {
    pub step: StepId,
    pub parent: TrajId,
    pub at_seq: Seq,
    pub children: Vec<TrajId>,
    /// `(child, parent, kind, at_seq)` for every edge in the store.
    pub edges: Vec<(TrajId, TrajId, EdgeKind, Seq)>,
    /// Trajectories carrying a `fork/end-seed` marker.
    pub end_seeds: Vec<TrajId>,
}

/// One `graph/merge`, likewise.
#[derive(Clone, Debug)]
pub struct MergeObs {
    pub step: StepId,
    pub head: TrajId,
    pub survivor_traj: TrajId,
    pub absorbed_traj: TrajId,
    pub absorbed: AgentName,
    /// `(child, parent)` of every MERGE edge in the store.
    pub merge_edges: Vec<(TrajId, TrajId)>,
    /// `src_trajs` of every reconciliation rollup on the head.
    pub reconciliations: Vec<Vec<TrajId>>,
    /// Whether an `agents` row still exists under the absorbed name.
    pub absorbed_row_exists: bool,
}

/// PURE: clause 1.
pub fn evaluate_splits(obs: &[SplitObs]) -> Result<(), String> {
    for o in obs {
        if o.children.len() != 2 {
            return Err(format!(
                "`graph/split` step `{}` names {} children; a split has exactly two",
                o.step,
                o.children.len()
            ));
        }
        for child in &o.children {
            let edges = o
                .edges
                .iter()
                .filter(|(c, p, k, s)| {
                    c == child && p == &o.parent && *k == EdgeKind::Ancestor && *s == o.at_seq
                })
                .count();
            if edges != 1 {
                return Err(format!(
                    "`graph/split` step `{}` has {edges} ancestor edge(s) `{child}` ← `{}` at \
                     seq {}; it must have exactly one per child",
                    o.step, o.parent, o.at_seq.0
                ));
            }
            if !o.end_seeds.contains(child) {
                return Err(format!(
                    "`graph/split` step `{}` names child `{child}`, which carries no \
                     `fork/end-seed` marker",
                    o.step
                ));
            }
        }
    }
    Ok(())
}

/// PURE: clauses 2 and 3.
pub fn evaluate_merges(obs: &[MergeObs]) -> Result<(), String> {
    for o in obs {
        for parent in [&o.survivor_traj, &o.absorbed_traj] {
            let n = o
                .merge_edges
                .iter()
                .filter(|(c, p)| c == &o.head && p == parent)
                .count();
            if n != 1 {
                return Err(format!(
                    "`graph/merge` step `{}` has {n} merge edge(s) `{}` ← `{parent}`; it must \
                     have exactly one per parent",
                    o.step, o.head
                ));
            }
        }
        let spanning = o
            .reconciliations
            .iter()
            .filter(|src| src.contains(&o.survivor_traj) && src.contains(&o.absorbed_traj))
            .count();
        if spanning != 1 {
            return Err(format!(
                "`graph/merge` step `{}` is covered by {spanning} reconciliation rollup(s) \
                 spanning `{}` and `{}`; it must have exactly one",
                o.step, o.survivor_traj, o.absorbed_traj
            ));
        }
        if o.absorbed_row_exists {
            return Err(format!(
                "`graph/merge` step `{}` absorbed `{}`, whose `agents` row still exists: §3 \
                 deletes the losing ROW (the trajectory stays)",
                o.step, o.absorbed
            ));
        }
    }
    Ok(())
}

/// Read every `graph/split` in the store, with the relations around it.
async fn splits(ctx: &Context) -> Result<Vec<SplitObs>, String> {
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        return Err("no `ledger` is bound to read the graph's relations from".into());
    };
    let steps = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new(GRAPH_SPLIT)],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("the ledger is unreadable: {e}"))?;
    let mut out = Vec::new();
    for s in steps {
        let body: GraphSplit = match serde_json::from_value((*s.body).clone()) {
            Ok(b) => b,
            Err(e) => return Err(format!("`graph/split` step `{}` is unreadable: {e}", s.id)),
        };
        let children: Vec<TrajId> = body.children.iter().map(|c| c.traj.clone()).collect();
        let mut edges = Vec::new();
        let mut end_seeds = Vec::new();
        for child in &children {
            for e in ledger
                .0
                .edges(child)
                .await
                .map_err(|e| format!("edges of `{child}` are unreadable: {e}"))?
            {
                edges.push((e.child, e.parent, e.kind, e.at_seq));
            }
            let seeds = ledger
                .0
                .steps(&StepQuery {
                    trajs: vec![child.clone()],
                    kinds: vec![StepType::new("fork/end-seed")],
                    ..Default::default()
                })
                .await
                .map_err(|e| format!("the ledger is unreadable: {e}"))?;
            if !seeds.is_empty() {
                end_seeds.push(child.clone());
            }
        }
        out.push(SplitObs {
            step: s.id,
            parent: body.parent,
            at_seq: body.at_seq,
            children,
            edges,
            end_seeds,
        });
    }
    Ok(out)
}

/// Read every `graph/merge` in the store, likewise.
async fn merges(ctx: &Context) -> Result<Vec<MergeObs>, String> {
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        return Err("no `ledger` is bound to read the graph's relations from".into());
    };
    let steps = ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new(GRAPH_MERGE)],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .map_err(|e| format!("the ledger is unreadable: {e}"))?;
    let mut out = Vec::new();
    for s in steps {
        let body: GraphMerge = match serde_json::from_value((*s.body).clone()) {
            Ok(b) => b,
            Err(e) => return Err(format!("`graph/merge` step `{}` is unreadable: {e}", s.id)),
        };
        let head = s.traj.clone();
        let merge_edges = ledger
            .0
            .edges(&head)
            .await
            .map_err(|e| format!("edges of `{head}` are unreadable: {e}"))?
            .into_iter()
            .filter(|e| e.kind == EdgeKind::Merge)
            .map(|e| (e.child, e.parent))
            .collect();
        let reconciliations = ledger
            .0
            .rollups(&RollupQuery {
                trajs: vec![head.clone()],
                kind: Some(RollupKind::Reconciliation),
                ..Default::default()
            })
            .await
            .map_err(|e| format!("rollups of `{head}` are unreadable: {e}"))?
            .into_iter()
            .map(|r| r.src_trajs)
            .collect();
        let absorbed_row_exists = ledger
            .0
            .agent(&body.absorbed)
            .await
            .map_err(|e| format!("the agents rows are unreadable: {e}"))?
            .is_some();
        out.push(MergeObs {
            step: s.id,
            head,
            survivor_traj: body.survivor_traj,
            absorbed_traj: body.absorbed_traj,
            absorbed: body.absorbed,
            merge_edges,
            reconciliations,
            absorbed_row_exists,
        });
    }
    Ok(out)
}

fn violation(name: &'static str, ctx: &Context, detail: String) -> InvariantViolation {
    InvariantViolation {
        invariant: name,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    }
}

/// Clause 1.
pub fn split_has_two_edges() -> InvariantSpec {
    InvariantSpec {
        name: "split_has_two_edges",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| {
            Box::pin(async move {
                let obs = splits(&ctx)
                    .await
                    .map_err(|d| violation("split_has_two_edges", &ctx, d))?;
                evaluate_splits(&obs).map_err(|d| violation("split_has_two_edges", &ctx, d))
            })
        },
    }
}

/// Clause 2.
pub fn merge_is_reconciled() -> InvariantSpec {
    InvariantSpec {
        name: "merge_is_reconciled",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| {
            Box::pin(async move {
                let mut obs = merges(&ctx)
                    .await
                    .map_err(|d| violation("merge_is_reconciled", &ctx, d))?;
                // Clause 3 is the other spec's business; this one judges edges and rollups only.
                for o in &mut obs {
                    o.absorbed_row_exists = false;
                }
                evaluate_merges(&obs).map_err(|d| violation("merge_is_reconciled", &ctx, d))
            })
        },
    }
}

/// Clause 3.
pub fn absorbed_row_is_gone() -> InvariantSpec {
    InvariantSpec {
        name: "absorbed_row_is_gone",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| {
            Box::pin(async move {
                let obs = merges(&ctx)
                    .await
                    .map_err(|d| violation("absorbed_row_is_gone", &ctx, d))?;
                for o in &obs {
                    if o.absorbed_row_exists {
                        return Err(violation(
                            "absorbed_row_is_gone",
                            &ctx,
                            format!(
                                "`graph/merge` step `{}` absorbed `{}`, whose `agents` row still \
                                 exists",
                                o.step, o.absorbed
                            ),
                        ));
                    }
                }
                Ok(())
            })
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> TrajId {
        TrajId::new(s)
    }

    fn clean_split() -> SplitObs {
        SplitObs {
            step: StepId::new("sp1"),
            parent: t("lane/sol"),
            at_seq: Seq(7),
            children: vec![t("lane/a"), t("lane/b")],
            edges: vec![
                (t("lane/a"), t("lane/sol"), EdgeKind::Ancestor, Seq(7)),
                (t("lane/b"), t("lane/sol"), EdgeKind::Ancestor, Seq(7)),
            ],
            end_seeds: vec![t("lane/a"), t("lane/b")],
        }
    }

    fn clean_merge() -> MergeObs {
        MergeObs {
            step: StepId::new("m1"),
            head: t("lane/sol+lane/terra"),
            survivor_traj: t("lane/sol"),
            absorbed_traj: t("lane/terra"),
            absorbed: AgentName::new("terra"),
            merge_edges: vec![
                (t("lane/sol+lane/terra"), t("lane/sol")),
                (t("lane/sol+lane/terra"), t("lane/terra")),
            ],
            reconciliations: vec![vec![t("lane/sol"), t("lane/terra")]],
            absorbed_row_exists: false,
        }
    }

    #[test]
    fn a_split_without_two_edges_is_reported() {
        let mut one_edge = clean_split();
        one_edge.edges.pop();
        let err = evaluate_splits(&[one_edge]).expect_err("a missing ancestor edge is reported");
        assert!(
            err.contains("lane/b") && err.contains("ancestor edge"),
            "{err}"
        );

        // The end-seed half of the same clause.
        let mut no_seed = clean_split();
        no_seed.end_seeds.pop();
        let err = evaluate_splits(&[no_seed]).expect_err("a missing end-seed is reported");
        assert!(err.contains("fork/end-seed"), "{err}");

        // And a split that names one child is not a split.
        let mut one_child = clean_split();
        one_child.children.pop();
        let err = evaluate_splits(&[one_child]).expect_err("a one-child split is reported");
        assert!(err.contains("exactly two"), "{err}");
    }

    #[test]
    fn a_merge_whose_absorbed_row_still_exists_is_reported() {
        let mut alive = clean_merge();
        alive.absorbed_row_exists = true;
        let err = evaluate_merges(&[alive]).expect_err("a surviving absorbed row is reported");
        assert!(
            err.contains("terra") && err.contains("still exists"),
            "{err}"
        );

        // The other half of clause 2: a merge with no reconciliation spanning both parents.
        let mut unreconciled = clean_merge();
        unreconciled.reconciliations = vec![vec![t("lane/sol")]];
        let err = evaluate_merges(&[unreconciled]).expect_err("an unreconciled merge is reported");
        assert!(err.contains("reconciliation"), "{err}");

        // And a missing merge edge.
        let mut one_edge = clean_merge();
        one_edge.merge_edges.pop();
        let err = evaluate_merges(&[one_edge]).expect_err("a missing merge edge is reported");
        assert!(err.contains("merge edge"), "{err}");
    }

    #[test]
    fn a_clean_stream_passes() {
        evaluate_splits(&[clean_split()]).expect("a split with two edges and two seeds is clean");
        evaluate_merges(&[clean_merge()]).expect("a reconciled merge with no losing row is clean");
        // Vacuously clean: a tree where no op ever ran.
        evaluate_splits(&[]).expect("no split, no violation");
        evaluate_merges(&[]).expect("no merge, no violation");
    }
}
