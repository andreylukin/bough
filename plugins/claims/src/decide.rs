//! Invariant: `decide` is the ONE place this phase's ground truth lives. It is the only writer of
//! `claim/accepted` and `claim/rejected`, the only caller of `pin/set` for a requirement, and the
//! only path from a claim to `ctx.graph`. A second writer anywhere would let a claim be accepted
//! without a pin, or a lane be born without an acceptance.
//!
//! - `Accept` on a `Requirement` ⇒ `claim/accepted { edited: false }` then `pin/set`.
//! - `Edit` ⇒ the same with `edited: true` and the EDITED text pinned; the proposal step is never
//!   rewritten — the edit is a new fact citing it.
//! - `Accept` on a `Lane` ⇒ `ctx.graph.apply(Bud { agent: Some(name) })` and `agents.resume`, in
//!   one transaction: a lane claim births a ROW and a LIVE agent, or neither.
//! - `Reject` ⇒ `claim/rejected { reason }` and nothing else, ever.

use std::collections::BTreeSet;

use bough_plugin_agents::ResumeAgent;
use bough_plugin_graph_ops::{
    BudRequest, ChildSpec, MergeRequest, OpOutcome, OpRequest, SplitRequest,
};
use bough_plugin_ledger::{
    AgentName, Append, Cite, Class, Ref, Seq, StepId, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_rollups::Attribution;

use crate::{
    kind::ClaimKind, pin, query, ClaimId, ClaimsError, ClaimsInner, DecideOutcome, DecideRequest,
    Decision, OpenClaim, ProposedChild,
};

/// The synthetic wake a decision is appended under (P4-D2's shape: a system pass runs under a
/// synthetic wake id). A decision is Andrey's act and belongs to no agent's wake.
pub fn decision_wake(claim: &ClaimId) -> WakeId {
    WakeId::new(format!("decide:{claim}"))
}

/// A `step:` cite of one step.
pub fn cite_step(step: &StepId) -> Cite {
    Cite {
        r#ref: Ref::new(format!("step:{step}")),
        url: None,
    }
}

/// The refusal §16 makes structural: a `decide` reached inside a wake is a wake accepting its own
/// claim. PURE in the sense that matters — it reads the ambient initiator and nothing else.
pub fn refuse_a_wake() -> Result<(), ClaimsError> {
    match bough_plugin_agents::initiator::current() {
        Some(by) => Err(ClaimsError::NotAndreysAct { by: by.to_string() }),
        None => Ok(()),
    }
}

/// The claim, and whether it has already been decided.
pub(crate) async fn load(
    inner: &ClaimsInner,
    claim: &ClaimId,
) -> Result<(OpenClaim, bool), ClaimsError> {
    let steps = inner
        .ledger
        .0
        .steps(&StepQuery {
            kinds: query::kinds(),
            ..Default::default()
        })
        .await?;
    let found = steps
        .iter()
        .filter(|s| s.kind.as_str() == "claim/proposed")
        .filter_map(query::as_claim)
        .find(|c| c.claim == *claim)
        .ok_or_else(|| ClaimsError::NoSuchClaim(claim.clone()))?;
    Ok((found, query::decided(&steps).contains(claim)))
}

/// The whole of §2.4's ground truth.
pub(crate) async fn run(
    inner: &ClaimsInner,
    req: DecideRequest,
) -> Result<DecideOutcome, ClaimsError> {
    refuse_a_wake()?;
    let crate::Actor::Andrey = req.actor;

    let (claim, already) = load(inner, &req.claim).await?;
    if already {
        return Err(ClaimsError::AlreadyDecided(req.claim.clone()));
    }
    let wake = decision_wake(&claim.claim);
    let cites = vec![cite_step(&claim.proposal)];

    match &req.decision {
        Decision::Reject { reason } => {
            let step = inner
                .ledger
                .0
                .append(Append {
                    traj: claim.traj.clone(),
                    wake,
                    kind: StepType::new(crate::rate::CLAIM_REJECTED),
                    class: Class::Thought,
                    body: serde_json::json!({
                        "claim": claim.claim.as_str(),
                        "proposal": claim.proposal,
                        "reason": reason,
                    }),
                    cites,
                    at: req.at,
                    id: None,
                })
                .await?;
            crate::invariant::record(crate::invariant::Obs {
                claim: claim.claim.clone(),
                accepted: false,
                requirement: matches!(claim.kind, ClaimKind::Requirement { .. }),
                pin: None,
            });
            Ok(DecideOutcome {
                claim: claim.claim.clone(),
                step: step.id,
                pin: None,
                graph: None,
                born: None,
            })
        }
        Decision::Accept | Decision::Edit { .. } => {
            let edited = matches!(req.decision, Decision::Edit { .. });
            let (title, text) = match &req.decision {
                Decision::Edit { title, body } => (title.clone(), body.clone()),
                _ => (claim.title.clone(), claim.body.clone()),
            };

            // STRUCTURE FIRST. A birth that fails must leave no acceptance behind, so nothing
            // durable about the decision is written until the structure exists.
            let (graph, born) = apply_structure(inner, &claim, &req).await?;

            let step = inner
                .ledger
                .0
                .append(Append {
                    traj: claim.traj.clone(),
                    wake: wake.clone(),
                    kind: StepType::new(crate::rate::CLAIM_ACCEPTED),
                    class: Class::Evidence,
                    body: serde_json::json!({
                        "claim": claim.claim.as_str(),
                        "proposal": claim.proposal,
                        "edited": edited,
                    }),
                    cites: cites.clone(),
                    at: req.at,
                    id: None,
                })
                .await?;

            let pin = match &claim.kind {
                ClaimKind::Requirement { supersedes } => {
                    let mut previous = supersedes.clone();
                    previous.extend(pins_of(inner, &claim).await?);
                    let pin_step = inner
                        .ledger
                        .0
                        .append(Append {
                            traj: claim.traj.clone(),
                            wake,
                            kind: StepType::new("pin/set"),
                            class: Class::Evidence,
                            body: serde_json::json!({
                                "title": title,
                                "text": text,
                                "supersedes": pin::supersedes_for(&previous),
                                // Which claim set this pin, so a later acceptance of the SAME
                                // requirement can supersede it without guessing by title.
                                "claim": claim.claim.as_str(),
                            }),
                            cites: vec![cite_step(&step.id)],
                            at: req.at,
                            id: None,
                        })
                        .await?;
                    Some(pin_step.id)
                }
                _ => None,
            };

            crate::invariant::record(crate::invariant::Obs {
                claim: claim.claim.clone(),
                accepted: true,
                requirement: matches!(claim.kind, ClaimKind::Requirement { .. }),
                pin: pin.clone(),
            });
            Ok(DecideOutcome {
                claim: claim.claim.clone(),
                step: step.id,
                pin,
                graph,
                born,
            })
        }
    }
}

/// Every pin this claim has set before, oldest first.
async fn pins_of(inner: &ClaimsInner, claim: &OpenClaim) -> Result<Vec<StepId>, ClaimsError> {
    let steps = inner
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![claim.traj.clone()],
            kinds: vec![StepType::new("pin/set")],
            ..Default::default()
        })
        .await?;
    Ok(steps
        .into_iter()
        .filter(|s| s.body.get("claim").and_then(|v| v.as_str()) == Some(claim.claim.as_str()))
        .map(|s| s.id)
        .collect())
}

/// The ONE path from a claim to `ctx.graph`.
async fn apply_structure(
    inner: &ClaimsInner,
    claim: &OpenClaim,
    req: &DecideRequest,
) -> Result<(Option<OpOutcome>, Option<AgentName>), ClaimsError> {
    let by = Attribution::Andrey;
    let cites = vec![cite_step(&claim.proposal)];
    let reason = format!("claim {} accepted: {}", claim.claim, claim.title);

    match &claim.kind {
        ClaimKind::Requirement { .. } | ClaimKind::Contradiction { .. } | ClaimKind::Other => {
            Ok((None, None))
        }
        ClaimKind::Lane {
            name,
            from_seq,
            routing_refs,
            wake_classes,
        } => {
            let at_seq = point(inner, &claim.traj, *from_seq).await?;
            let outcome = inner
                .graph
                .0
                .apply(&OpRequest::Bud(BudRequest {
                    parent: claim.by.clone(),
                    at_seq,
                    child: ChildSpec {
                        agent: Some(name.clone()),
                        traj: lane_traj(name),
                        routing_refs: routing_refs.clone(),
                        wake_classes: wake_classes.clone(),
                    },
                    reason,
                    by,
                    cites,
                    at: req.at,
                }))
                .await?;
            // A row without a resident is a lane nobody is living in: if the resume fails, the
            // row goes with it, and the acceptance is never written (§2.4: a row AND a live
            // agent, or neither).
            match inner
                .agents
                .resume(ResumeAgent {
                    name: name.clone(),
                    at: req.at,
                    setup: None,
                })
                .await
            {
                Ok((_agent, disposer)) => {
                    // The disposer is the teardown CAPABILITY (§2). The born lane outlives this
                    // call, so it is dropped rather than fired; dropping disposes nothing.
                    drop(disposer);
                    Ok((Some(outcome), Some(name.clone())))
                }
                Err(e) => {
                    inner.ledger.0.delete_agent(name).await?;
                    Err(ClaimsError::Other(anyhow::anyhow!(
                        "lane `{name}` could not be brought up: {e}"
                    )))
                }
            }
        }
        ClaimKind::Split(p) => {
            let at_seq = point(inner, &claim.traj, p.at_seq).await?;
            let children = p
                .children
                .iter()
                .enumerate()
                .map(|(i, c)| child_spec(claim, i, c))
                .collect();
            let outcome = inner
                .graph
                .0
                .apply(&OpRequest::Split(SplitRequest {
                    parent: p.parent.clone(),
                    at_seq: Some(at_seq),
                    children,
                    reason,
                    by,
                    cites,
                    at: req.at,
                }))
                .await?;
            Ok((Some(outcome), None))
        }
        ClaimKind::Merge(p) => {
            // §4: a merge needs a survivor NAMED by Andrey. The absence of one is a question,
            // never a default, so it is refused here rather than resolved.
            let survivor = p
                .survivor
                .clone()
                .ok_or(bough_plugin_graph_ops::GraphError::NoSurvivor)?;
            let outcome = inner
                .graph
                .0
                .apply(&OpRequest::Merge(MergeRequest {
                    survivor,
                    absorbed: p.absorbed.clone(),
                    reason,
                    by,
                    cites,
                    at: req.at,
                }))
                .await?;
            Ok((Some(outcome), None))
        }
        ClaimKind::Bud(p) => {
            let outcome = inner
                .graph
                .0
                .apply(&OpRequest::Bud(BudRequest {
                    parent: p.parent.clone(),
                    at_seq: p.at_seq,
                    child: child_spec(claim, 0, &p.child),
                    reason,
                    by,
                    cites,
                    at: req.at,
                }))
                .await?;
            Ok((Some(outcome), None))
        }
    }
}

/// The trajectory a born lane lives on.
pub fn lane_traj(name: &AgentName) -> TrajId {
    TrajId::new(format!("lane/{name}"))
}

/// A proposed child, as the graph seam takes it. A child with no name is HEADLESS (§4's fork) and
/// its trajectory is named after the claim, so two claims cannot mint the same id.
fn child_spec(claim: &OpenClaim, i: usize, c: &ProposedChild) -> ChildSpec {
    ChildSpec {
        agent: c.agent.clone(),
        traj: match &c.agent {
            Some(name) => lane_traj(name),
            None => TrajId::new(format!("fork/{}-{i}", claim.claim)),
        },
        routing_refs: c.routing_refs.clone(),
        wake_classes: c.wake_classes.clone(),
    }
}

/// The point an op branches at: the claim's own, else the parent's head (P5-D7 leaves the
/// open-wake question to the graph seam, which owns it).
async fn point(inner: &ClaimsInner, traj: &TrajId, named: Option<Seq>) -> Result<Seq, ClaimsError> {
    match named {
        Some(s) => Ok(s),
        None => Ok(inner.ledger.0.head_seq(traj).await?.unwrap_or(Seq(0))),
    }
}

/// The routing refs a lane claim carries, for the caller that wants them without re-parsing.
pub fn routing_of(kind: &ClaimKind) -> BTreeSet<Ref> {
    match kind {
        ClaimKind::Lane { routing_refs, .. } => routing_refs.clone(),
        _ => BTreeSet::new(),
    }
}
