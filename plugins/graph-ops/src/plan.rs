//! Invariant: [`GraphOps::plan`] is PURE with respect to the world — it reads the ledger, calls no
//! model and writes nothing — and it is TOTAL: every child is either planned or named in
//! `questions`. A plan that silently omits a child is a plan that would half-apply.

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, EdgeKind, Ref, RollupId, Seq, StepId, TrajId};
use bough_plugin_rollups::Attribution;
use chrono::{DateTime, Utc};

use crate::route::RoutingPlan;

/// Which op a plan or an outcome is about.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum OpKind {
    Split,
    Merge,
    Bud,
    Fork,
    Undo,
}

/// One op, as a caller asks for it.
#[derive(Clone, Debug)]
pub enum OpRequest {
    Split(SplitRequest),
    Merge(MergeRequest),
    Bud(BudRequest),
    Fork(ForkRequest),
}

/// One new branch a split or a bud creates.
#[derive(Clone, Debug, PartialEq)]
pub struct ChildSpec {
    /// `None` ⇒ a HEADLESS branch: a trajectory and an ancestor edge, no `agents` row (§4's
    /// fork). Promotable later by adding the row and nothing else.
    pub agent: Option<AgentName>,
    pub traj: TrajId,
    pub routing_refs: BTreeSet<Ref>,
    pub wake_classes: BTreeSet<String>,
}

/// A split: two heads from one, at the parent's head.
#[derive(Clone, Debug)]
pub struct SplitRequest {
    pub parent: AgentName,
    /// `None` ⇒ the parent's head, resolved to the last seq outside an open wake (P5-D7).
    pub at_seq: Option<Seq>,
    /// Exactly two. Each names the new lane and the refs it takes with it.
    pub children: Vec<ChildSpec>,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<bough_plugin_ledger::Cite>,
    pub at: DateTime<Utc>,
}

/// A bud: a split at a PAST point, and the parent never pauses (§4).
#[derive(Clone, Debug)]
pub struct BudRequest {
    pub parent: AgentName,
    /// The PAST point. Mandatory: a bud whose point is the head is a split.
    pub at_seq: Seq,
    pub child: ChildSpec,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<bough_plugin_ledger::Cite>,
    pub at: DateTime<Utc>,
}

/// A fork: a bud with no `agents` row and no routing.
#[derive(Clone, Debug)]
pub struct ForkRequest {
    pub parent: AgentName,
    pub at_seq: Option<Seq>,
    pub traj: TrajId,
    pub reason: String,
    pub by: Attribution,
    pub at: DateTime<Utc>,
}

/// A merge: two lanes into one surviving row.
#[derive(Clone, Debug)]
pub struct MergeRequest {
    /// ANDREY'S CHOICE. Never inferred; the absence of one is a leader question, not a default.
    pub survivor: AgentName,
    pub absorbed: AgentName,
    pub reason: String,
    pub by: Attribution,
    pub cites: Vec<bough_plugin_ledger::Cite>,
    pub at: DateTime<Utc>,
}

/// An undo of a previous `graph/split` or `graph/bud`.
#[derive(Clone, Debug)]
pub struct UndoRequest {
    /// The `graph/split` or `graph/bud` step being undone.
    pub of: StepId,
    pub by: Attribution,
    pub at: DateTime<Utc>,
}

/// What an op WOULD write.
#[derive(Clone, Debug, PartialEq)]
pub struct OpPlan {
    pub kind: OpKind,
    pub at_seq: Seq,
    pub new_trajs: Vec<TrajId>,
    pub edges: Vec<(TrajId, TrajId, EdgeKind)>,
    pub digests: Vec<DigestPlan>,
    pub routing: RoutingPlan,
    /// Non-empty ⇒ `apply` refuses and `ask_leader` is the caller's next move.
    pub questions: Vec<String>,
}

/// One digest an op would ask `ctx.rollups` for. `reconcile` selects P5-D13's `recon:` namespace.
#[derive(Clone, Debug, PartialEq)]
pub struct DigestPlan {
    pub traj: TrajId,
    pub parents: Vec<TrajId>,
    pub reconcile: bool,
}

/// What an op DID.
#[derive(Clone, Debug, PartialEq)]
pub struct OpOutcome {
    pub kind: OpKind,
    /// The cited op step (`graph/split` | `graph/merge` | `graph/bud` | `graph/undo`), appended
    /// LAST (P5-D8).
    pub step: StepId,
    pub trajs: Vec<TrajId>,
    pub edges: usize,
    pub digests: Vec<RollupId>,
    pub rows_written: Vec<AgentName>,
    pub rows_deleted: Vec<AgentName>,
    /// `Pointers` did no summarising; `Merge` produced a reconciliation digest.
    pub undo_shape: Option<UndoShape>,
}

/// The two shapes §4's undo rules allow.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum UndoShape {
    /// The children were never lived in: delete the rows, restore the refs, no model call.
    Pointers,
    /// A child has steps beyond its `fork/end-seed`: run the merge path, which reconciles.
    Merge,
}

/// The trajectory a merge's new HEAD is: deterministic, so `plan` and `apply` name the same one
/// and a re-run after a crash does not mint a second head.
pub fn merge_head(survivor: &TrajId, absorbed: &TrajId) -> TrajId {
    TrajId::new(format!("{survivor}+{absorbed}"))
}

/// PURE over what the caller hands it: the plan for one request, given the parent's chain and the
/// rows in play. TOTAL: every child is either planned or named in `questions`.
pub fn plan_for(
    req: &OpRequest,
    at_seq: Seq,
    rows: &[bough_plugin_ledger::AgentRow],
    cfg: &crate::GraphConfig,
) -> OpPlan {
    let row = |n: &AgentName| rows.iter().find(|r| &r.name == n).cloned();
    match req {
        OpRequest::Split(r) => {
            let mut questions = Vec::new();
            let Some(parent) = row(&r.parent) else {
                return refused(OpKind::Split, at_seq, vec![no_such(&r.parent)]);
            };
            if r.children.len() != cfg.max_children {
                questions.push(format!(
                    "a split of `{}` takes exactly {} children; {} were named",
                    r.parent,
                    cfg.max_children,
                    r.children.len()
                ));
            }
            branch_plan(OpKind::Split, at_seq, &parent, &r.children, cfg, questions)
        }
        OpRequest::Bud(r) => {
            let Some(parent) = row(&r.parent) else {
                return refused(OpKind::Bud, at_seq, vec![no_such(&r.parent)]);
            };
            let kind = if r.child.agent.is_none() {
                OpKind::Fork
            } else {
                OpKind::Bud
            };
            branch_plan(
                kind,
                at_seq,
                &parent,
                std::slice::from_ref(&r.child),
                cfg,
                Vec::new(),
            )
        }
        OpRequest::Fork(r) => {
            let Some(parent) = row(&r.parent) else {
                return refused(OpKind::Fork, at_seq, vec![no_such(&r.parent)]);
            };
            let child = ChildSpec {
                agent: None,
                traj: r.traj.clone(),
                routing_refs: Default::default(),
                wake_classes: Default::default(),
            };
            branch_plan(
                OpKind::Fork,
                at_seq,
                &parent,
                std::slice::from_ref(&child),
                cfg,
                Vec::new(),
            )
        }
        OpRequest::Merge(r) => {
            // ANDREY'S CHOICE (§4). An unnamed survivor is a QUESTION, never a default: picking
            // one by rule would silently delete a lane nobody chose to lose.
            if r.survivor.as_str().is_empty() {
                return refused(
                    OpKind::Merge,
                    at_seq,
                    vec![format!(
                        "which lane survives the merge with `{}`? a merge needs a survivor named \
                         by Andrey",
                        r.absorbed
                    )],
                );
            }
            let (Some(s), Some(a)) = (row(&r.survivor), row(&r.absorbed)) else {
                let missing = if row(&r.survivor).is_none() {
                    &r.survivor
                } else {
                    &r.absorbed
                };
                return refused(OpKind::Merge, at_seq, vec![no_such(missing)]);
            };
            let head = merge_head(&s.traj, &a.traj);
            OpPlan {
                kind: OpKind::Merge,
                at_seq,
                new_trajs: vec![head.clone()],
                edges: vec![
                    (head.clone(), s.traj.clone(), EdgeKind::Merge),
                    (head.clone(), a.traj.clone(), EdgeKind::Merge),
                ],
                digests: vec![DigestPlan {
                    traj: head,
                    parents: vec![s.traj.clone(), a.traj.clone()],
                    reconcile: true,
                }],
                routing: crate::route::plan_merge(&s, &a),
                questions: Vec::new(),
            }
        }
    }
}

/// The shared body of split, bud and fork: the same edges, the same digests, the same routing
/// rule. One implementation, so "a bud is a split at a past point" is a fact about the code.
fn branch_plan(
    kind: OpKind,
    at_seq: Seq,
    parent: &bough_plugin_ledger::AgentRow,
    children: &[ChildSpec],
    cfg: &crate::GraphConfig,
    mut questions: Vec<String>,
) -> OpPlan {
    let verdict = crate::route::plan_split(&parent.routing_refs, children);
    let routing = match verdict {
        crate::route::RoutingVerdict::Settled(p) => p,
        crate::route::RoutingVerdict::Ambiguous(amb) => {
            for a in &amb {
                questions.push(format!(
                    "`{}` is claimed by {} — which lane routes it?",
                    a.r#ref,
                    a.claimed_by
                        .iter()
                        .map(|n| format!("`{n}`"))
                        .collect::<Vec<_>>()
                        .join(" and ")
                ));
            }
            // A refused verdict still leaves the parent holding everything: nothing moves while
            // the question is open.
            crate::route::RoutingPlan {
                assign: Vec::new(),
                keep: parent.routing_refs.clone(),
            }
        }
    };
    let digests = children
        .iter()
        .filter(|c| c.agent.is_some() || cfg.digest_on_fork)
        .map(|c| DigestPlan {
            traj: c.traj.clone(),
            parents: vec![parent.traj.clone()],
            reconcile: false,
        })
        .collect();
    OpPlan {
        kind,
        at_seq,
        new_trajs: children.iter().map(|c| c.traj.clone()).collect(),
        edges: children
            .iter()
            .map(|c| (c.traj.clone(), parent.traj.clone(), EdgeKind::Ancestor))
            .collect(),
        digests,
        routing,
        questions,
    }
}

fn no_such(name: &AgentName) -> String {
    format!("there is no agent named `{name}`")
}

/// A plan that would write nothing, carrying the reasons.
fn refused(kind: OpKind, at_seq: Seq, questions: Vec<String>) -> OpPlan {
    OpPlan {
        kind,
        at_seq,
        new_trajs: Vec::new(),
        edges: Vec::new(),
        digests: Vec::new(),
        routing: crate::route::RoutingPlan {
            assign: Vec::new(),
            keep: Default::default(),
        },
        questions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{AgentRow, Ref};

    fn cfg() -> crate::GraphConfig {
        crate::GraphConfig {
            max_children: 2,
            digest_on_fork: false,
            question_on_ambiguity: true,
        }
    }

    fn refs(v: &[&str]) -> BTreeSet<Ref> {
        v.iter().map(|s| Ref::new(*s)).collect()
    }

    fn parent_row() -> AgentRow {
        AgentRow {
            name: AgentName::new("sol"),
            traj: TrajId::new("lane/sol"),
            routing_refs: refs(&["gh:o/r", "slack:c1"]),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        }
    }

    fn child(name: &str, rs: &[&str]) -> ChildSpec {
        ChildSpec {
            agent: Some(AgentName::new(name)),
            traj: TrajId::new(format!("lane/{name}")),
            routing_refs: refs(rs),
            wake_classes: Default::default(),
        }
    }

    fn split(children: Vec<ChildSpec>) -> OpRequest {
        OpRequest::Split(SplitRequest {
            parent: AgentName::new("sol"),
            at_seq: None,
            children,
            reason: "two concerns".into(),
            by: Attribution::Andrey,
            cites: vec![],
            at: chrono::Utc::now(),
        })
    }

    #[test]
    fn a_plan_is_total_every_child_is_planned_or_questioned() {
        let req = split(vec![child("a", &["gh:o/r"]), child("b", &["slack:c1"])]);
        let plan = plan_for(&req, Seq(7), &[parent_row()], &cfg());
        assert!(plan.questions.is_empty());
        // Every child appears once as a trajectory, once as an edge, once as a digest and once
        // in the routing. A child missing from any of the four would half-apply.
        assert_eq!(plan.new_trajs.len(), 2);
        assert_eq!(plan.edges.len(), 2);
        assert_eq!(plan.digests.len(), 2);
        assert_eq!(plan.routing.assign.len(), 2);
        for c in ["a", "b"] {
            let traj = TrajId::new(format!("lane/{c}"));
            assert!(plan.new_trajs.contains(&traj));
            assert!(plan.edges.contains(&(
                traj.clone(),
                TrajId::new("lane/sol"),
                EdgeKind::Ancestor
            )));
            assert!(plan.digests.iter().any(|d| d.traj == traj
                && d.parents == vec![TrajId::new("lane/sol")]
                && !d.reconcile));
            assert!(plan
                .routing
                .assign
                .iter()
                .any(|(n, _)| n == &AgentName::new(c)));
        }
        assert_eq!(plan.at_seq, Seq(7));
    }

    #[test]
    fn a_plan_with_questions_names_every_ambiguous_ref() {
        let req = split(vec![
            child("a", &["gh:o/r", "slack:c1"]),
            child("b", &["gh:o/r", "slack:c1"]),
        ]);
        let plan = plan_for(&req, Seq(7), &[parent_row()], &cfg());
        assert_eq!(plan.questions.len(), 2, "{:?}", plan.questions);
        for r in ["gh:o/r", "slack:c1"] {
            assert!(
                plan.questions.iter().any(|q| q.contains(r)),
                "`{r}` is contested and unnamed: {:?}",
                plan.questions
            );
        }
        // Nothing moves while the question is open: the parent still holds every ref.
        assert!(plan.routing.assign.is_empty());
        assert_eq!(plan.routing.keep, refs(&["gh:o/r", "slack:c1"]));
    }

    /// §4: a merge's survivor is Andrey's choice, so an unnamed one is a question and the plan
    /// writes nothing.
    #[test]
    fn a_merge_without_a_survivor_is_a_question() {
        let req = OpRequest::Merge(MergeRequest {
            survivor: AgentName::new(""),
            absorbed: AgentName::new("terra"),
            reason: "one lane is enough".into(),
            by: Attribution::Andrey,
            cites: vec![],
            at: chrono::Utc::now(),
        });
        let plan = plan_for(&req, Seq(1), &[parent_row()], &cfg());
        assert_eq!(plan.questions.len(), 1);
        assert!(plan.questions[0].contains("terra"));
        assert!(plan.new_trajs.is_empty() && plan.edges.is_empty());
    }
}
