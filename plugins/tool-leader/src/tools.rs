//! Invariant: every tool here registers at `ToolScope::Agent(target)` where `target` comes from
//! `ctx.leader.target()` and NEVER from this row's own config (P5-D10). Two rows with two
//! spellings of one target is a misconfiguration that would present as "half the leader set
//! moved"; injecting the key makes the move atomic.
//!
//! THE CLAIMS DEMOLITION (2026-08-30, Andrey's call): structure no longer waits on anyone.
//! `create_lane` and `merge_lanes` apply through `ctx.graph` DIRECTLY — cited Evidence, attributed
//! to the leader, reversible through `graph/undo` — where the old `propose_claim` wrote a
//! `claim/proposed` for Andrey to accept. Headlong (§18's reference) lets an agent spawn
//! trajectories without bound; a lane is dearer than a fork (it wakes, it costs), so the price of
//! the open hand is the CLEANUP DUTY the persona carries and the roster the projection shows the
//! leader every wake. `curate` is unchanged: `adopt_unsorted` + `note_timeline` in one journalled
//! pass.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_agents::{AgentsHandle, ResumeAgent};
use bough_plugin_graph_ops::{BudRequest, ChildSpec, GraphHandle, MergeRequest, OpRequest};
use bough_plugin_leader::{AdoptRequest, LeaderHandle, TimelineEntry};
use bough_plugin_ledger::{
    AgentName, Cite, LedgerHandle, Order, Ref, Seq, StepId, StepQuery, StepType, TrajId,
};
use bough_plugin_rollups::Attribution;
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};

/// The three tool names, in registration order.
pub const TOOL_NAMES: [&str; 3] = ["create_lane", "merge_lanes", "curate"];

/// The trajectory a born lane lives on (the `residents` row's `traj_prefix` spelling).
pub fn lane_traj(name: &AgentName) -> TrajId {
    TrajId::new(format!("lane/{name}"))
}

/// Register all three for the leader's target.
pub async fn register(ctx: &Context, leader: &LeaderHandle) -> Result<(), PluginError> {
    let entry = ctx.entry_id().clone();
    let err = |e: bough_kernel::KernelError| PluginError::new(entry.clone(), e);
    let tools = ctx.get::<Tools>().map_err(err)?;
    let graph = (*ctx.get::<bough_plugin_graph_ops::Graph>().map_err(err)?).clone();
    let agents = (*ctx.get::<bough_plugin_agents::Agents>().map_err(err)?).clone();
    let ledger = (*ctx.get::<bough_plugin_ledger::Ledger>().map_err(err)?).clone();
    // ONE read of the binding, and every spec built from it: the target cannot be half-moved.
    let target = leader.target().clone();

    for spec in specs(&target, leader, &graph, &agents, &ledger) {
        tools.register(ctx, spec).await?;
    }
    Ok(())
}

/// The three specs, all scoped to `target`.
pub fn specs(
    target: &AgentName,
    leader: &LeaderHandle,
    graph: &GraphHandle,
    agents: &AgentsHandle,
    ledger: &LedgerHandle,
) -> Vec<ToolSpec> {
    let scoped =
        |name: &str, description: &str, schema: serde_json::Value, tool: Arc<dyn Tool>| ToolSpec {
            name: ToolName::new(name),
            description: description.to_string(),
            input_schema: schemars::Schema::try_from(schema).expect("a literal object schema"),
            render: RenderIntent::Generic,
            scope: ToolScope::Agent(target.clone()),
            tool,
        };
    vec![
        scoped(
            "create_lane",
            "Create a new lane, live immediately: an `agents` row, a trajectory budded from \
             yours, and mail routed on `routing_refs` from this moment on. Lanes are yours to \
             open freely — and yours to clean up: fold a finished or quiet lane back with \
             `merge_lanes`. Andrey can reverse a creation with `/undo`.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "name": { "type": "string" },
                    "reason": { "type": "string" },
                    "routing_refs": { "type": "array", "items": { "type": "string" } },
                    "wake_classes": { "type": "array", "items": { "type": "string" } },
                    "cites": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["name", "reason"]
            }),
            Arc::new(CreateLane {
                leader: leader.clone(),
                graph: graph.clone(),
                agents: agents.clone(),
                ledger: ledger.clone(),
            }),
        ),
        scoped(
            "merge_lanes",
            "Fold one lane into another: the absorbed lane's row leaves the rail and its mail \
             re-routes to the survivor, while its trajectory stays readable forever. This is the \
             cleanup half of `create_lane` — use it when a lane's work is done, has gone quiet, \
             or belongs inside another lane's.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "survivor": { "type": "string" },
                    "absorbed": { "type": "string" },
                    "reason": { "type": "string" },
                    "cites": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["survivor", "absorbed", "reason"]
            }),
            Arc::new(MergeLanes {
                leader: leader.clone(),
                graph: graph.clone(),
            }),
        ),
        scoped(
            "curate",
            "Tidy the population in one pass: place items off the unsorted mail queue with \
             `placements` (an item you do not place stays on the queue), and/or note moments on \
             the cross-agent timeline with `timeline`. A timeline entry is evidence, so it must \
             cite what it is a reading of.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "placements": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "step": { "type": "string" },
                                "agent": { "type": "string" }
                            },
                            "required": ["step", "agent"]
                        }
                    },
                    "steps": { "type": "array", "items": { "type": "string" } },
                    "timeline": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string" },
                                "at": { "type": "string" },
                                "agents": { "type": "array", "items": { "type": "string" } },
                                "refs": { "type": "array", "items": { "type": "string" } },
                                "cites": { "type": "array", "items": { "type": "string" } }
                            },
                            "required": ["title", "at", "cites"]
                        }
                    }
                }
            }),
            Arc::new(Curate(leader.clone())),
        ),
    ]
}

// ---- create_lane -------------------------------------------------------------------------------

/// The direct half of what `claims::decide` used to do on an accepted lane claim: bud the
/// trajectory, then bring the resident up — a row AND a live agent, or neither.
struct CreateLane {
    leader: LeaderHandle,
    graph: GraphHandle,
    agents: AgentsHandle,
    ledger: LedgerHandle,
}

/// PURE: what one `create_lane` call asks for.
#[derive(Debug, PartialEq)]
pub struct LaneRequest {
    pub name: AgentName,
    pub reason: String,
    pub routing_refs: std::collections::BTreeSet<Ref>,
    pub wake_classes: std::collections::BTreeSet<String>,
}

/// PURE: read a `create_lane` call. A blank name is refused here, with the tool's own words,
/// rather than deeper down with the graph's.
pub fn lane_request(args: &serde_json::Value) -> Result<LaneRequest, ToolFailure> {
    let name = str_arg(args, "name")?;
    if name.trim().is_empty() || name.contains(char::is_whitespace) {
        return Err(ToolFailure {
            kind: FailureClass::Error,
            message: "`name` must be one word: it names the rail row and the `lane/<name>` \
                      trajectory"
                .to_string(),
        });
    }
    Ok(LaneRequest {
        name: AgentName::new(&name),
        reason: str_arg(args, "reason")?,
        routing_refs: strings(args, "routing_refs")
            .into_iter()
            .map(|s| Ref::new(&s))
            .collect(),
        wake_classes: strings(args, "wake_classes").into_iter().collect(),
    })
}

/// The cites a direct op carries: whatever the call named, plus the call itself — so the
/// Evidence step is never uncited even when the model cited nothing.
pub fn op_cites(call: &ToolCall) -> Vec<Cite> {
    let mut out = cites(&call.args);
    out.push(Cite {
        r#ref: Ref::new(format!("call:{}", call.id)),
        url: None,
    });
    out
}

#[async_trait::async_trait]
impl Tool for CreateLane {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let req = lane_request(&call.args)?;
        let at = chrono::Utc::now();
        let target = self.leader.target().clone();
        // A lane that exists is reported as itself, never re-budded: the second call is a no-op
        // with a message, so a retried wake cannot mint `lane/x` twice.
        if self
            .ledger
            .0
            .agent(&req.name)
            .await
            .map_err(|e| failed(e.to_string()))?
            .is_some()
        {
            return Err(ToolFailure {
                kind: FailureClass::Error,
                message: format!("a lane named `{}` already exists", req.name),
            });
        }
        let traj = self
            .leader
            .traj()
            .await
            .map_err(|e| failed(e.to_string()))?;
        // The bud point must lie OUTSIDE the open wake — and this call is happening INSIDE one
        // (the tool runs in the leader's own wake; found live: `head_seq` was refused with
        // "seq N lies inside the open wake"). The seam's own PURE resolver walks down to the
        // last legal seq, over the same wake-vocabulary chain the seam itself reads; a chain
        // that is nothing but the open first wake resolves to the origin.
        let chain = self
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj.clone()],
                kinds: bough_plugin_graph_ops::seq::WAKE_KINDS
                    .iter()
                    .map(StepType::new)
                    .collect(),
                order: Order::SeqDesc,
                ..Default::default()
            })
            .await
            .map_err(|e| failed(e.to_string()))?;
        let head = self
            .ledger
            .0
            .head_seq(&traj)
            .await
            .map_err(|e| failed(e.to_string()))?
            .unwrap_or(Seq(0));
        let at_seq = bough_plugin_graph_ops::resolve_point(head, &chain).unwrap_or(Seq(0));
        let by = Attribution::Agent {
            name: target.clone(),
        };
        let outcome = self
            .graph
            .0
            .apply(&OpRequest::Bud(BudRequest {
                parent: target,
                at_seq,
                child: ChildSpec {
                    agent: Some(req.name.clone()),
                    traj: lane_traj(&req.name),
                    routing_refs: req.routing_refs.clone(),
                    wake_classes: req.wake_classes.clone(),
                },
                reason: req.reason.clone(),
                by: by.clone(),
                cites: op_cites(&call),
                at,
            }))
            .await
            .map_err(|e| failed(e.to_string()))?;
        // A row without a resident is a lane nobody is living in (§2.4, moved verbatim from
        // `claims::decide`): if the resume fails, the bud is UNDONE through the operation that
        // exists for it, and the undo is itself a truthful record.
        match self
            .agents
            .resume(ResumeAgent {
                name: req.name.clone(),
                at,
                setup: None,
            })
            .await
        {
            Ok((_agent, disposer)) => {
                // The teardown capability (§2): the born lane outlives this call, so it is
                // dropped rather than fired; dropping disposes nothing.
                drop(disposer);
            }
            // The row's own reconciler (`agents/rows-changed` → `residents`) may have brought the
            // lane up between the bud and here. That IS the outcome this branch wants.
            Err(bough_plugin_agents::AgentError::AlreadyLive(_)) => {}
            Err(e) => {
                if let Err(ue) = self
                    .graph
                    .0
                    .undo(&bough_plugin_graph_ops::UndoRequest {
                        of: outcome.step.clone(),
                        by,
                        at,
                    })
                    .await
                {
                    tracing::error!(
                        agent = %req.name,
                        error = %ue,
                        "create_lane: the lane could not be brought up AND the bud could not be \
                         undone; the row is deleted and the bud step stands"
                    );
                }
                // Belt and braces: the undo deletes the row, but a failed undo must not leave a
                // row with nobody in it.
                if self
                    .ledger
                    .0
                    .agent(&req.name)
                    .await
                    .map_err(|e| failed(e.to_string()))?
                    .is_some()
                {
                    self.ledger
                        .0
                        .delete_agent(&req.name)
                        .await
                        .map_err(|e| failed(e.to_string()))?;
                }
                return Err(failed(format!(
                    "lane `{}` could not be brought up: {e}",
                    req.name
                )));
            }
        }
        Ok(ToolOutcome {
            content: format!(
                "lane `{}` is live, routed on [{}] — fold it back with merge_lanes when its work \
                 is done",
                req.name,
                req.routing_refs
                    .iter()
                    .map(|r| r.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            value: Some(serde_json::json!({
                "lane": req.name.as_str(),
                "step": outcome.step.as_str(),
            })),
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

// ---- merge_lanes -------------------------------------------------------------------------------

struct MergeLanes {
    leader: LeaderHandle,
    graph: GraphHandle,
}

/// PURE: read a `merge_lanes` call. Absorbing yourself is refused here: the leader folding its
/// own lane away would take the leader set down with it.
pub fn merge_request(
    args: &serde_json::Value,
    target: &AgentName,
) -> Result<MergeRequest, ToolFailure> {
    let survivor = AgentName::new(&str_arg(args, "survivor")?);
    let absorbed = AgentName::new(&str_arg(args, "absorbed")?);
    if survivor == absorbed {
        return Err(ToolFailure {
            kind: FailureClass::Error,
            message: "`survivor` and `absorbed` must be two different lanes".to_string(),
        });
    }
    if absorbed == *target {
        return Err(ToolFailure {
            kind: FailureClass::Error,
            message: "you cannot absorb your own lane: the leader set is mounted on it".to_string(),
        });
    }
    Ok(MergeRequest {
        survivor,
        absorbed,
        reason: str_arg(args, "reason")?,
        by: Attribution::Agent {
            name: target.clone(),
        },
        cites: Vec::new(),
        at: chrono::Utc::now(),
    })
}

#[async_trait::async_trait]
impl Tool for MergeLanes {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let mut req = merge_request(&call.args, self.leader.target())?;
        req.cites = op_cites(&call);
        let survivor = req.survivor.clone();
        let absorbed = req.absorbed.clone();
        let outcome = self
            .graph
            .0
            .apply(&OpRequest::Merge(req))
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!(
                "lane `{absorbed}` folded into `{survivor}`; its trajectory stays readable"
            ),
            value: Some(serde_json::json!({
                "survivor": survivor.as_str(),
                "absorbed": absorbed.as_str(),
                "step": outcome.step.as_str(),
            })),
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

// ---- curate ------------------------------------------------------------------------------------

/// `adopt_unsorted` + `note_timeline`, in one call.
struct Curate(LeaderHandle);

/// PURE: what one `curate` call asks for.
#[derive(Debug, Default)]
pub struct CurateRequest {
    pub placements: Vec<(StepId, AgentName)>,
    pub steps: Option<Vec<StepId>>,
    pub timeline: Vec<TimelineEntry>,
}

impl CurateRequest {
    /// A call that asks for NOTHING. Refused rather than reported as a successful no-op: a
    /// silent success would read to the model as "the queue was empty".
    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
            && self.timeline.is_empty()
            && self.steps.as_ref().is_none_or(|s| s.is_empty())
    }
}

/// PURE: read a `curate` call. An unreadable `at` is an error, as it was in `note_timeline`.
pub fn curate_request(args: &serde_json::Value) -> Result<CurateRequest, ToolFailure> {
    let placements = args
        .get("placements")
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    Some((
                        StepId::new(p.get("step")?.as_str()?),
                        AgentName::new(p.get("agent")?.as_str()?),
                    ))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let steps = args.get("steps").and_then(|v| v.as_array()).map(|a| {
        a.iter()
            .filter_map(|v| v.as_str())
            .map(StepId::new)
            .collect::<Vec<_>>()
    });
    let mut timeline = Vec::new();
    for e in args
        .get("timeline")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default()
    {
        let at_text = str_arg(&e, "at")?;
        let at = chrono::DateTime::parse_from_rfc3339(&at_text)
            .map_err(|err| ToolFailure {
                kind: FailureClass::Error,
                message: format!("`at` must be an RFC3339 moment: {err}"),
            })?
            .with_timezone(&chrono::Utc);
        timeline.push(TimelineEntry {
            title: str_arg(&e, "title")?,
            at,
            agents: strings(&e, "agents")
                .into_iter()
                .map(|s| AgentName::new(&s))
                .collect(),
            refs: strings(&e, "refs")
                .into_iter()
                .map(|s| Ref::new(&s))
                .collect(),
            cites: cites(&e),
        });
    }
    let req = CurateRequest {
        placements,
        steps,
        timeline,
    };
    if req.is_empty() {
        return Err(ToolFailure {
            kind: FailureClass::Error,
            message: "curate needs something to do: `placements`, `steps` or `timeline`"
                .to_string(),
        });
    }
    Ok(req)
}

#[async_trait::async_trait]
impl Tool for Curate {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let req = curate_request(&call.args)?;
        let adopt_asked = !req.placements.is_empty() || req.steps.is_some();
        let report = if adopt_asked {
            Some(
                self.0
                    .adopt(AdoptRequest {
                        steps: req.steps,
                        placements: req.placements,
                        at: chrono::Utc::now(),
                    })
                    .await
                    .map_err(|e| failed(e.to_string()))?,
            )
        } else {
            None
        };
        let mut noted = Vec::new();
        for entry in req.timeline {
            noted.push(
                self.0
                    .note_timeline(entry)
                    .await
                    .map_err(|e| failed(e.to_string()))?,
            );
        }

        let mut parts = Vec::new();
        if let Some(r) = &report {
            parts.push(format!(
                "adopted {} item(s); {} held on the queue",
                r.adopted.len(),
                r.held.len()
            ));
        }
        if !noted.is_empty() {
            parts.push(format!("noted {} moment(s) on the timeline", noted.len()));
        }
        Ok(ToolOutcome {
            content: parts.join("; "),
            value: Some(serde_json::json!({
                "adopted": report.as_ref().map(|r| r.adopted.iter().map(|(s, a)| serde_json::json!({
                    "step": s.as_str(), "agent": a.as_str()
                })).collect::<Vec<_>>()).unwrap_or_default(),
                "held": report.as_ref().map(|r| r.held.iter().map(|s| s.as_str()).collect::<Vec<_>>()).unwrap_or_default(),
                "timeline": noted.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            })),
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

// ---- shared argument reading -----------------------------------------------------------------

fn strings(args: &serde_json::Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(|v| v.as_array())
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(|s| s.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Cites arrive as bare refs: a model writes `step:<id>`, never a `{ref, url}` object.
fn cites(args: &serde_json::Value) -> Vec<Cite> {
    strings(args, "cites")
        .into_iter()
        .map(|s| Cite {
            r#ref: Ref::new(&s),
            url: None,
        })
        .collect()
}

fn str_arg(args: &serde_json::Value, key: &str) -> Result<String, ToolFailure> {
    args.get(key)
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| ToolFailure {
            kind: FailureClass::Error,
            message: format!("`{key}` is required"),
        })
}

fn failed(message: String) -> ToolFailure {
    ToolFailure {
        kind: FailureClass::Error,
        message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lane_request_reads_its_refs_and_refuses_a_blank_name() {
        let req = lane_request(&serde_json::json!({
            "name": "infra",
            "reason": "the infra mail deserves a lane",
            "routing_refs": ["repo:bough"],
            "wake_classes": ["class:ask"]
        }))
        .expect("a well-formed lane");
        assert_eq!(req.name, AgentName::new("infra"));
        assert!(req.routing_refs.contains(&Ref::new("repo:bough")));
        assert!(lane_request(&serde_json::json!({ "name": " ", "reason": "r" })).is_err());
        assert!(lane_request(&serde_json::json!({ "name": "two words", "reason": "r" })).is_err());
        assert!(
            lane_request(&serde_json::json!({ "name": "x" })).is_err(),
            "reason is required"
        );
    }

    #[test]
    fn a_merge_refuses_self_absorption_and_a_degenerate_pair() {
        let target = AgentName::new("sol");
        let err = merge_request(
            &serde_json::json!({ "survivor": "terra", "absorbed": "sol", "reason": "r" }),
            &target,
        )
        .expect_err("absorbing the leader's own lane");
        assert!(err.message.contains("your own lane"), "{}", err.message);
        let err = merge_request(
            &serde_json::json!({ "survivor": "terra", "absorbed": "terra", "reason": "r" }),
            &target,
        )
        .expect_err("a merge of a lane into itself");
        assert!(err.message.contains("two different"), "{}", err.message);
        let ok = merge_request(
            &serde_json::json!({ "survivor": "sol", "absorbed": "terra", "reason": "folded" }),
            &target,
        )
        .expect("an ordinary merge");
        assert_eq!(ok.survivor, AgentName::new("sol"));
        assert!(matches!(ok.by, Attribution::Agent { ref name } if *name == target));
    }

    #[test]
    fn cites_are_read_as_bare_refs() {
        let c = cites(&serde_json::json!({ "cites": ["step:s1", "gh:o/r#1"] }));
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].r#ref, Ref::new("step:s1"));
        assert!(c[0].url.is_none());
    }

    #[test]
    fn an_empty_curate_is_refused() {
        let err = curate_request(&serde_json::json!({})).expect_err("an empty call does nothing");
        assert!(err.message.contains("something to do"), "{}", err.message);
        let err = curate_request(&serde_json::json!({ "placements": [], "timeline": [] }))
            .expect_err("nor do empty arrays");
        assert!(err.message.contains("something to do"), "{}", err.message);
    }

    #[test]
    fn curate_reads_each_half() {
        let req = curate_request(&serde_json::json!({
            "placements": [{ "step": "s1", "agent": "terra" }],
            "timeline": [{ "title": "t", "at": "2026-01-01T00:00:00Z", "cites": ["step:s1"] }]
        }))
        .expect("both halves");
        assert_eq!(
            req.placements,
            vec![(StepId::new("s1"), AgentName::new("terra"))]
        );
        assert_eq!(req.timeline.len(), 1);
    }
}
