//! Invariant (§4): the parent NEVER pauses. A bud branches at a past seq and touches no step of
//! the parent's chain, so a wake running on the parent completes untouched and its consumed set
//! stays intact. A bud with `agent: None` is a FORK: a trajectory and an ancestor edge, no row and
//! no routing, promotable later by adding the row and nothing else.

use crate::plan::{BudRequest, ChildSpec, ForkRequest, OpKind, OpOutcome, OpRequest};
use crate::split::{branch, Branch};
use crate::{GraphError, GraphInner};

/// A bud: a branch at a PAST point. The point is MANDATORY and taken as given — a bud whose point
/// is the head is a split, and this op never silently becomes one.
pub async fn apply(inner: &GraphInner, req: &BudRequest) -> Result<OpOutcome, GraphError> {
    let kind = if req.child.agent.is_none() {
        OpKind::Fork
    } else {
        OpKind::Bud
    };
    branch(
        inner,
        Branch {
            kind,
            request: OpRequest::Bud(req.clone()),
            parent: req.parent.clone(),
            at_seq: Some(req.at_seq),
            children: vec![req.child.clone()],
            reason: req.reason.clone(),
            by: req.by.clone(),
            cites: req.cites.clone(),
            at: req.at,
        },
    )
    .await
}

/// A fork: a bud with no `agents` row and no routing (§4). Nothing else differs — which is why
/// promoting one is adding the row and nothing else.
pub async fn apply_fork(inner: &GraphInner, req: &ForkRequest) -> Result<OpOutcome, GraphError> {
    let child = ChildSpec {
        agent: None,
        traj: req.traj.clone(),
        routing_refs: Default::default(),
        wake_classes: Default::default(),
    };
    branch(
        inner,
        Branch {
            kind: OpKind::Fork,
            request: OpRequest::Fork(req.clone()),
            parent: req.parent.clone(),
            at_seq: req.at_seq,
            children: vec![child],
            reason: req.reason.clone(),
            by: req.by.clone(),
            cites: Vec::new(),
            at: req.at,
        },
    )
    .await
}
