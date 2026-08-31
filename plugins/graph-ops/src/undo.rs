//! Invariant (§4's undo rules): an UNUSED split undoes as POINTERS — delete the child rows,
//! restore the parent's refs from the op step, append `graph/undo`, and call no model at all. A
//! LIVED-IN one undoes as a MERGE with the parent as survivor, which writes the reconciliation
//! digest and leaves the divergent heads behind by construction, because no trajectory is ever
//! deleted.

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, Append, Cite, Class, Ref, Seq, StepId, StepType, TrajId};

use crate::plan::{MergeRequest, OpKind, OpOutcome, UndoRequest, UndoShape};
use crate::vocabulary::{
    ChildRecord, GraphBud, GraphSplit, GraphUndo, GRAPH_BUD, GRAPH_SPLIT, GRAPH_UNDO,
};
use crate::{GraphError, GraphInner};

/// What the undone op wrote, read back from its own step.
struct Undone {
    parent: TrajId,
    children: Vec<ChildRecord>,
}

pub async fn apply(inner: &GraphInner, req: &UndoRequest) -> Result<OpOutcome, GraphError> {
    let step = inner
        .ledger
        .0
        .step(&req.of)
        .await?
        .ok_or_else(|| GraphError::NotAnOp(req.of.clone()))?;
    let undone = match step.kind.as_str() {
        GRAPH_SPLIT => {
            let b: GraphSplit = serde_json::from_value((*step.body).clone())
                .map_err(|e| GraphError::Other(anyhow::anyhow!("unreadable graph/split: {e}")))?;
            Undone {
                parent: b.parent,
                children: b.children,
            }
        }
        GRAPH_BUD => {
            let b: GraphBud = serde_json::from_value((*step.body).clone())
                .map_err(|e| GraphError::Other(anyhow::anyhow!("unreadable graph/bud: {e}")))?;
            Undone {
                parent: b.parent,
                children: vec![ChildRecord {
                    traj: b.child,
                    agent: b.agent,
                    routing_refs: b.routing_refs,
                    digest: None,
                }],
            }
        }
        _ => return Err(GraphError::NotAnOp(req.of.clone())),
    };

    let parent_name = inner.name_of(&undone.parent).await?;
    // "Lived in" is a fact about the LEDGER, not about intent: a child whose chain is nothing but
    // its `fork/end-seed` marker (seq 1) has no history anyone would lose.
    let mut lived_in: Vec<&ChildRecord> = Vec::new();
    let mut unused: Vec<&ChildRecord> = Vec::new();
    for c in &undone.children {
        let head = inner.ledger.0.head_seq(&c.traj).await?.unwrap_or(Seq(0));
        if head > Seq(1) {
            lived_in.push(c);
        } else {
            unused.push(c);
        }
    }

    let trajs: Vec<TrajId> = undone.children.iter().map(|c| c.traj.clone()).collect();
    let mut digests = Vec::new();
    let mut rows_deleted: Vec<AgentName> = Vec::new();
    let mut rows_written: Vec<AgentName> = Vec::new();
    // Counted, never assumed: each absorbed child is a full merge, and a two-child split whose
    // children were both lived in writes twice what a single merge does.
    let mut edges = 0usize;

    if lived_in.is_empty() {
        // POINTERS. No digest, no model call, nothing summarised: there is nothing to reconcile.
        for c in &unused {
            if let Some(name) = &c.agent {
                inner.ledger.0.delete_agent(name).await?;
                rows_deleted.push(name.clone());
            }
        }
        let mut parent = inner.row(&parent_name).await?;
        let restored: BTreeSet<Ref> = parent
            .routing_refs
            .iter()
            .cloned()
            .chain(
                undone
                    .children
                    .iter()
                    .flat_map(|c| c.routing_refs.iter().cloned()),
            )
            .collect();
        parent.routing_refs = restored;
        inner.ledger.0.put_agent(parent).await?;
        rows_written.push(parent_name.clone());
        let step = write_undo(
            inner,
            req,
            &undone.parent,
            UndoShape::Pointers,
            &trajs,
            &req.of,
        )
        .await?;
        return Ok(OpOutcome {
            kind: OpKind::Undo,
            step,
            trajs,
            edges: 0,
            digests,
            rows_written,
            rows_deleted,
            undo_shape: Some(UndoShape::Pointers),
        });
    }

    // MERGE. The parent is the survivor; each lived-in child is absorbed. Its trajectory stays
    // exactly where it is — a divergent head left behind, and named in the undo step below.
    for c in lived_in {
        let Some(name) = &c.agent else { continue };
        let survivor = inner.row(&parent_name).await?;
        let absorbed = inner.row(name).await?;
        let mreq = MergeRequest {
            survivor: survivor.name.clone(),
            absorbed: absorbed.name.clone(),
            reason: format!("undo of `{}`", req.of),
            by: req.by.clone(),
            cites: vec![Cite {
                r#ref: Ref::step(&req.of),
                url: None,
            }],
            at: req.at,
        };
        let out = crate::merge::merge_rows(
            inner,
            &survivor,
            &absorbed,
            &mreq.reason.clone(),
            &mreq,
            mreq.cites.clone(),
        )
        .await?;
        digests.extend(out.digests);
        rows_written.extend(out.rows_written);
        rows_deleted.extend(out.rows_deleted);
        edges += out.edges;
    }
    for c in &unused {
        if let Some(name) = &c.agent {
            inner.ledger.0.delete_agent(name).await?;
            rows_deleted.push(name.clone());
        }
    }
    // The parent's trajectory MOVED under the merge, so the undo step lands on the new head.
    let parent_traj = inner.row(&parent_name).await?.traj;
    let step = write_undo(inner, req, &parent_traj, UndoShape::Merge, &trajs, &req.of).await?;
    Ok(OpOutcome {
        kind: OpKind::Undo,
        step,
        trajs,
        edges,
        digests,
        rows_written,
        rows_deleted,
        undo_shape: Some(UndoShape::Merge),
    })
}

async fn write_undo(
    inner: &GraphInner,
    req: &UndoRequest,
    traj: &TrajId,
    shape: UndoShape,
    trajs: &[TrajId],
    of: &StepId,
) -> Result<StepId, GraphError> {
    let step = inner
        .ledger
        .0
        .append(Append {
            traj: traj.clone(),
            wake: crate::op_wake(),
            kind: StepType::new(GRAPH_UNDO),
            class: Class::Evidence,
            body: serde_json::to_value(GraphUndo {
                of: of.clone(),
                shape,
                trajs: trajs.to_vec(),
                by: req.by.clone(),
            })
            .expect("a GraphUndo serialises"),
            cites: vec![Cite {
                r#ref: Ref::step(of),
                url: None,
            }],
            at: req.at,
            id: None,
        })
        .await?;
    Ok(step.id)
}
