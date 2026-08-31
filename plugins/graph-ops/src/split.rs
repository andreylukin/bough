//! Invariant (P5-D8): the cited `graph/split` step is appended LAST. A crash mid-op leaves an
//! orphan trajectory and an edge nothing names — inert, invisible to `connected()` for any agent
//! without a row, and the op is simply re-runnable. Appending the op step first would leave a
//! cited FACT naming trajectories that do not exist, which is the failure mode §16 cares about.
//!
//! Order, identical for split, bud and fork: resolve the seq → plan → `ledger.fork` per child →
//! one inheritance digest per child through `ctx.rollups` → `put_agent` the children then the
//! reduced parent → append the cited step.

use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Cite, Class, Fork, Ref, Seq, StepType, TrajId,
};
use bough_plugin_rollups::{Attribution, DigestRequest};
use chrono::{DateTime, Utc};

use crate::plan::{ChildSpec, OpKind, OpOutcome, OpRequest, SplitRequest};
use crate::vocabulary::{ChildRecord, GraphBud, GraphSplit, GRAPH_BUD, GRAPH_SPLIT};
use crate::{GraphError, GraphInner};

/// A split: two heads from one, at the parent's head.
pub async fn apply(inner: &GraphInner, req: &SplitRequest) -> Result<OpOutcome, GraphError> {
    if req.children.len() != crate::SPLIT_CHILDREN {
        return Err(GraphError::ChildCount {
            expected: crate::SPLIT_CHILDREN,
            got: req.children.len(),
        });
    }
    branch(
        inner,
        Branch {
            kind: OpKind::Split,
            request: OpRequest::Split(req.clone()),
            parent: req.parent.clone(),
            at_seq: req.at_seq,
            children: req.children.clone(),
            reason: req.reason.clone(),
            by: req.by.clone(),
            cites: req.cites.clone(),
            at: req.at,
        },
    )
    .await
}

/// One branching op, whatever it is called. Split, bud and fork differ in the STEP they append
/// and in how the point is chosen; everything between is one implementation, so "a bud is a split
/// at a past point" (§4) is a fact about the code rather than a comment.
pub struct Branch {
    pub kind: OpKind,
    pub request: OpRequest,
    pub parent: AgentName,
    pub at_seq: Option<Seq>,
    pub children: Vec<ChildSpec>,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
}

pub async fn branch(inner: &GraphInner, b: Branch) -> Result<OpOutcome, GraphError> {
    let parent = inner.row(&b.parent).await?;
    // 1. The point, resolved or refused — never clipped, never waited on (P5-D7).
    let at_seq = inner.resolve_point(&parent, b.at_seq).await?;

    // 2. The plan. A question here is a REFUSAL: nothing is written while it is open (§4).
    let rows = inner.ledger.0.agents().await?;
    let plan = crate::plan::plan_for(&b.request, at_seq, &rows, &inner.cfg);
    if !plan.questions.is_empty() {
        return inner.refuse(&plan.questions, b.cites.clone(), b.at).await;
    }

    // 3. One fork per child: the ancestor edge and the `fork/end-seed` marker, in one ledger
    //    transaction each. The PAST is not partitioned — both children read the parent's chain
    //    through `ancestry`, and no step of it moves.
    let mut records: Vec<ChildRecord> = Vec::new();
    let mut seed_cites: Vec<Cite> = Vec::new();
    let mut digests = Vec::new();
    let mut rows_written = Vec::new();
    for child in &b.children {
        let out = inner
            .ledger
            .0
            .fork(Fork {
                parent: parent.traj.clone(),
                child: child.traj.clone(),
                at_seq,
                at: b.at,
            })
            .await?;
        seed_cites.push(Cite {
            r#ref: Ref::step(&out.end_seed.id),
            url: None,
        });

        // 4. The inheritance digest — the ONLY model call an op makes, and it is made through the
        //    Phase 4 seam. A headless fork has no row to carry one, so it gets one only when the
        //    row's `digest_on_fork` says so.
        let digest = if child.agent.is_some() || inner.cfg.digest_on_fork {
            let report = inner
                .rollups
                .0
                .rebuild_digest(&DigestRequest {
                    agent: child.agent.clone().unwrap_or_else(|| parent.name.clone()),
                    traj: child.traj.clone(),
                    at: b.at,
                    attribution: b.by.clone(),
                    from_raw: false,
                    parents: vec![parent.traj.clone()],
                    reconcile: false,
                })
                .await?;
            digests.push(report.digest.clone());
            Some(report.digest)
        } else {
            None
        };

        // 5. The child's row, if it has one. A fork has none, and takes no routing with it.
        if let Some(name) = &child.agent {
            inner
                .ledger
                .0
                .put_agent(AgentRow {
                    name: name.clone(),
                    traj: child.traj.clone(),
                    routing_refs: plan.routing.refs_of(name),
                    wake_classes: child.wake_classes.clone(),
                    model_override: parent.model_override.clone(),
                    tick_floor: parent.tick_floor,
                    digest_rollup: None,
                })
                .await?;
            rows_written.push(name.clone());
        }
        records.push(ChildRecord {
            traj: child.traj.clone(),
            agent: child.agent.clone(),
            routing_refs: child
                .agent
                .as_ref()
                .map(|n| plan.routing.refs_of(n).into_iter().collect::<Vec<Ref>>())
                .unwrap_or_default(),
            digest,
        });
    }

    // The parent keeps what nobody claimed. A bud/fork claims nothing, so this is a no-op write
    // for them and the reduction for a split.
    let mut reduced = parent.clone();
    reduced.routing_refs = plan.routing.keep.clone();
    if reduced.routing_refs != parent.routing_refs {
        inner.ledger.0.put_agent(reduced).await?;
        rows_written.push(parent.name.clone());
    }

    // 6. The cited step, LAST. It cites what the caller cited AND every end-seed it created, so
    //    the fact names its own evidence even when the caller offered none.
    let mut cites = b.cites.clone();
    cites.extend(seed_cites);
    let (kind, body) = match b.kind {
        OpKind::Split => (
            GRAPH_SPLIT,
            serde_json::to_value(GraphSplit {
                parent: parent.traj.clone(),
                at_seq,
                children: records.clone(),
                reason: b.reason.clone(),
                by: b.by.clone(),
            })
            .expect("a GraphSplit serialises"),
        ),
        _ => {
            let one = records.first().expect("a bud has exactly one child");
            (
                GRAPH_BUD,
                serde_json::to_value(GraphBud {
                    parent: parent.traj.clone(),
                    child: one.traj.clone(),
                    at_seq,
                    agent: one.agent.clone(),
                    routing_refs: one.routing_refs.clone(),
                    reason: b.reason.clone(),
                    by: b.by.clone(),
                })
                .expect("a GraphBud serialises"),
            )
        }
    };
    let step = inner
        .ledger
        .0
        .append(Append {
            traj: parent.traj.clone(),
            wake: crate::op_wake(),
            kind: StepType::new(kind),
            class: Class::Evidence,
            body,
            cites,
            at: b.at,
            id: None,
        })
        .await?;

    Ok(OpOutcome {
        kind: b.kind,
        step: step.id,
        trajs: records
            .iter()
            .map(|r| r.traj.clone())
            .collect::<Vec<TrajId>>(),
        edges: records.len(),
        digests,
        rows_written,
        rows_deleted: Vec::new(),
        undo_shape: None,
    })
}
