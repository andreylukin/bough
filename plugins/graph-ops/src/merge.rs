//! Invariant (§3): a merge deletes the losing ROW and keeps BOTH trajectories. Two
//! [`bough_plugin_ledger::EdgeKind::Merge`] edges point into ONE new head; one reconciliation
//! digest spans both parents; the survivor's row takes the UNION of `routing_refs` and its OWN
//! `model_override` / `tick_floor` / `wake_classes`. Nothing is ever deleted from the past —
//! sealed tiers on both parents stay valid because neither trajectory moved.

use bough_plugin_ledger::{Append, Cite, Class, Edge, EdgeKind, Ref, Seq, StepType};
use bough_plugin_rollups::DigestRequest;

use crate::plan::{merge_head, MergeRequest, OpKind, OpOutcome, OpRequest};
use crate::vocabulary::{GraphMerge, GRAPH_MERGE};
use crate::{GraphError, GraphInner};

pub async fn apply(inner: &GraphInner, req: &MergeRequest) -> Result<OpOutcome, GraphError> {
    // §4: ANDREY'S CHOICE. An unnamed survivor is a question, never a coin toss.
    if req.survivor.as_str().is_empty() {
        let plan = crate::plan::plan_for(
            &OpRequest::Merge(req.clone()),
            Seq(0),
            &inner.ledger.0.agents().await?,
            &inner.cfg,
        );
        // The question is asked; the refusal is the SPECIFIC one, so the caller learns what is
        // missing rather than the generic "ambiguous".
        let _ = inner
            .refuse::<OpOutcome>(&plan.questions, req.cites.clone(), req.at)
            .await;
        return Err(GraphError::NoSurvivor);
    }
    let survivor = inner.row(&req.survivor).await?;
    let absorbed = inner.row(&req.absorbed).await?;
    merge_rows(
        inner,
        &survivor,
        &absorbed,
        &req.reason,
        req,
        req.cites.clone(),
    )
    .await
}

/// The merge path, shared with `undo`'s lived-in case (§4: undoing a lived-in split IS a merge).
pub async fn merge_rows(
    inner: &GraphInner,
    survivor: &bough_plugin_ledger::AgentRow,
    absorbed: &bough_plugin_ledger::AgentRow,
    reason: &str,
    req: &MergeRequest,
    cites: Vec<Cite>,
) -> Result<OpOutcome, GraphError> {
    let head = merge_head(&survivor.traj, &absorbed.traj);
    let s_head = inner.head(&survivor.traj).await?;
    let a_head = inner.head(&absorbed.traj).await?;

    // Two MERGE edges into ONE new head. Neither parent trajectory moves, so every sealed tier on
    // either of them stays exactly as valid as it was.
    //
    // DEVIATION from the plan's letter, and the reason for it: `connected()` derives membership
    // from ANCESTOR edges only (§3, frozen in Phase 1), so a head joined to its parents by merge
    // edges alone would read NEITHER past — the survivor would lose its own history the moment it
    // merged. The head therefore carries both kinds to each parent: the merge edge is the FACT of
    // the merge, the ancestor edge is what makes the past readable.
    let mut edges = 0usize;
    for (parent, at_seq) in [(&survivor.traj, s_head), (&absorbed.traj, a_head)] {
        for kind in [EdgeKind::Merge, EdgeKind::Ancestor] {
            inner
                .ledger
                .0
                .add_edge(Edge {
                    child: head.clone(),
                    parent: parent.clone(),
                    at_seq,
                    kind,
                    at: req.at,
                })
                .await?;
            edges += 1;
        }
    }

    // ONE reconciliation digest spanning BOTH parents, through the Phase 4 seam (P5-D13).
    let recon = inner
        .rollups
        .0
        .rebuild_digest(&DigestRequest {
            agent: survivor.name.clone(),
            traj: head.clone(),
            at: req.at,
            attribution: req.by.clone(),
            from_raw: false,
            parents: vec![survivor.traj.clone(), absorbed.traj.clone()],
            reconcile: true,
        })
        .await?;

    // The surviving row: the union of the refs and the wake classes, the SURVIVOR's overrides,
    // and the new head as its trajectory.
    let mut row = crate::route::merged_row(survivor, absorbed);
    row.traj = head.clone();
    inner.ledger.0.put_agent(row).await?;
    // The losing ROW goes; both trajectories stay (§3).
    inner.ledger.0.delete_agent(&absorbed.name).await?;

    let mut cites = cites;
    cites.push(Cite {
        r#ref: Ref::rollup(&recon.digest),
        url: None,
    });
    let step = inner
        .ledger
        .0
        .append(Append {
            traj: head.clone(),
            wake: crate::op_wake(),
            kind: StepType::new(GRAPH_MERGE),
            class: Class::Evidence,
            body: serde_json::to_value(GraphMerge {
                survivor: survivor.name.clone(),
                absorbed: absorbed.name.clone(),
                survivor_traj: survivor.traj.clone(),
                absorbed_traj: absorbed.traj.clone(),
                at_seq: s_head,
                reconciliation: recon.digest.clone(),
                reason: reason.to_string(),
                by: req.by.clone(),
            })
            .expect("a GraphMerge serialises"),
            cites,
            at: req.at,
            id: None,
        })
        .await?;

    Ok(OpOutcome {
        kind: OpKind::Merge,
        step: step.id,
        trajs: vec![head],
        edges,
        digests: vec![recon.digest],
        rows_written: vec![survivor.name.clone()],
        rows_deleted: vec![absorbed.name.clone()],
        undo_shape: None,
    })
}
