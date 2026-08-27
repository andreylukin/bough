//! Invariant: all five tools register at `ToolScope::Agent(target)` where `target` comes from
//! `ctx.leader.target()` and NEVER from this row's own config (P5-D10). Two rows with two
//! spellings of one target is a misconfiguration that would present as "half the leader set
//! moved"; injecting the key makes the move atomic.
//!
//! `propose_claim` SHADOWS the global one from `claims`: it accepts the structural kinds the
//! global one refuses. That is V6's shadowing subject and a real difference in behaviour.
//!
//! And every one of them PROPOSES or CURATES. `propose_structure` writes `claim/proposed`, never
//! an op: there is no path from this crate to `ctx.graph`.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_claims::{kind, ClaimKind, ClaimsHandle, ProposeRequest};
use bough_plugin_leader::{AdoptRequest, DraftRequest, LeaderHandle, TimelineEntry};
use bough_plugin_ledger::{AgentName, Cite, Ref, StepId};
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};

/// The five tool names, in registration order.
pub const TOOL_NAMES: [&str; 5] = [
    "propose_claim",
    "adopt_unsorted",
    "draft_requirement",
    "propose_structure",
    "note_timeline",
];

/// The kinds the SCOPED `propose_claim` admits — the global three plus the structural four.
pub const LEADER_KINDS: [&str; 7] = [
    "requirement",
    "contradiction",
    "other",
    "lane",
    "split",
    "merge",
    "bud",
];

/// The kinds `propose_structure` admits. `lane` is here too: a new lane IS structure.
pub const STRUCTURAL_KINDS: [&str; 4] = ["lane", "split", "merge", "bud"];

/// Register all five for the leader's target.
pub async fn register(ctx: &Context, leader: &LeaderHandle) -> Result<(), PluginError> {
    let entry = ctx.entry_id().clone();
    let tools = ctx
        .get::<Tools>()
        .map_err(|e| PluginError::new(entry.clone(), e))?;
    let claims = ctx
        .get::<bough_plugin_claims::Claims>()
        .map_err(|e| PluginError::new(entry, e))?;
    // ONE read of the binding, and every spec built from it: the target cannot be half-moved.
    let target = leader.target().clone();

    for spec in specs(&target, leader, &claims) {
        tools.register(ctx, spec).await?;
    }
    Ok(())
}

/// The five specs, all scoped to `target`.
pub fn specs(target: &AgentName, leader: &LeaderHandle, claims: &ClaimsHandle) -> Vec<ToolSpec> {
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
            "propose_claim",
            "Propose a claim. As the leader you may also propose the structural kinds — lane, \
             split, merge, bud — which an ordinary lane may not. Andrey decides.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": LEADER_KINDS },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "detail": { "type": "object" }
                },
                "required": ["kind", "title", "body"]
            }),
            Arc::new(ProposeClaim {
                claims: claims.clone(),
                leader: leader.clone(),
                structural_only: false,
            }),
        ),
        scoped(
            "adopt_unsorted",
            "Read the unsorted mail queue and place each item with a lane, or leave it. An item \
             you do not place stays on the queue.",
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
                    "steps": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["placements"]
            }),
            Arc::new(AdoptUnsorted(leader.clone())),
        ),
        scoped(
            "draft_requirement",
            "Write down a requirement you heard from Andrey, as a claim citing his words. It \
             becomes binding only when he accepts it.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "cites": { "type": "array", "items": { "type": "string" } },
                    "supersedes": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["title", "body", "cites"]
            }),
            Arc::new(DraftRequirement(leader.clone())),
        ),
        scoped(
            "propose_structure",
            "Propose a change to the shape of the tree: a split, a merge, a bud, or a new lane. \
             This proposes; it never performs one.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": STRUCTURAL_KINDS },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "detail": { "type": "object" }
                },
                "required": ["kind", "title", "body", "detail"]
            }),
            Arc::new(ProposeClaim {
                claims: claims.clone(),
                leader: leader.clone(),
                structural_only: true,
            }),
        ),
        scoped(
            "note_timeline",
            "Note one moment on the cross-agent timeline. It is evidence, so it must cite what it \
             is a reading of.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "title": { "type": "string" },
                    "at": { "type": "string" },
                    "agents": { "type": "array", "items": { "type": "string" } },
                    "refs": { "type": "array", "items": { "type": "string" } },
                    "cites": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["title", "at", "cites"]
            }),
            Arc::new(NoteTimeline(leader.clone())),
        ),
    ]
}

// ---- the tools -------------------------------------------------------------------------------

/// The shadowing `propose_claim`, and `propose_structure`: one implementation, one flag.
///
/// They differ only in which kinds they admit, and neither refuses a structural kind the way the
/// global tool does. Two copies of the propose path would be two places for "the leader may
/// propose structure" to drift.
struct ProposeClaim {
    claims: ClaimsHandle,
    /// The trajectory a proposal lands on comes from the BINDING's target, not from `call.agent`:
    /// these tools are in the target's scope and nobody else's, so the two are the same name and
    /// reading it from the binding needs no second ledger dependency.
    leader: LeaderHandle,
    /// `true` ⇒ a non-structural kind is refused: `propose_structure` is about structure.
    structural_only: bool,
}

/// PURE: the kind a call asks for. UNLIKE the global tool, a structural name is admitted.
pub fn kind_of(args: &serde_json::Value, structural_only: bool) -> Result<ClaimKind, ToolFailure> {
    let name = args.get("kind").and_then(|v| v.as_str()).unwrap_or("other");
    if structural_only && !kind::is_structural_name(name) {
        return Err(ToolFailure {
            kind: FailureClass::Denied,
            message: format!(
                "`{name}` is not a structural kind; propose_structure takes one of {}",
                STRUCTURAL_KINDS.join(", ")
            ),
        });
    }
    // The structured half rides `detail`, which is exactly where `claims::kind::parse` reads it
    // from a stored body — so a kind proposed here reads back as the same kind.
    let detail = match args.get("detail") {
        Some(d @ serde_json::Value::Object(_)) => d.clone(),
        _ => args.clone(),
    };
    let parsed = kind::parse(name, &serde_json::json!({ kind::DETAIL_KEY: detail }));
    if structural_only && !parsed.is_structural() {
        return Err(ToolFailure {
            kind: FailureClass::Error,
            message: format!(
                "`{name}` needs a `detail` this binary can read; it parsed as `{}`",
                parsed.as_str()
            ),
        });
    }
    Ok(parsed)
}

#[async_trait::async_trait]
impl Tool for ProposeClaim {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let kind = kind_of(&call.args, self.structural_only)?;
        let traj = self
            .leader
            .traj()
            .await
            .map_err(|e| failed(e.to_string()))?;
        let claim = self
            .claims
            .propose(ProposeRequest {
                by: call.agent.clone(),
                traj,
                wake: Some(call.wake.clone()),
                kind,
                title: str_arg(&call.args, "title")?,
                body: str_arg(&call.args, "body")?,
                cites: cites(&call.args),
                at: chrono::Utc::now(),
            })
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!(
                "proposed claim {} ({}): {}",
                claim.claim,
                claim.kind.as_str(),
                claim.title
            ),
            value: Some(serde_json::json!({ "claim": claim.claim.as_str() })),
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

struct AdoptUnsorted(LeaderHandle);

#[async_trait::async_trait]
impl Tool for AdoptUnsorted {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let placements = call
            .args
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
        let steps = call.args.get("steps").and_then(|v| v.as_array()).map(|a| {
            a.iter()
                .filter_map(|v| v.as_str())
                .map(StepId::new)
                .collect()
        });
        let report = self
            .0
            .adopt(AdoptRequest {
                steps,
                placements,
                at: chrono::Utc::now(),
            })
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!(
                "adopted {} item(s); {} held on the queue",
                report.adopted.len(),
                report.held.len()
            ),
            value: Some(serde_json::json!({
                "adopted": report.adopted.iter().map(|(s, a)| serde_json::json!({
                    "step": s.as_str(), "agent": a.as_str()
                })).collect::<Vec<_>>(),
                "held": report.held.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
            })),
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

struct DraftRequirement(LeaderHandle);

#[async_trait::async_trait]
impl Tool for DraftRequirement {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let from = cites(&call.args);
        if from.is_empty() {
            return Err(ToolFailure {
                kind: FailureClass::Error,
                message: "a requirement drafted from Andrey's words must cite them".to_string(),
            });
        }
        let traj = self.0.traj().await.map_err(|e| failed(e.to_string()))?;
        let claim = self
            .0
            .draft_requirement(DraftRequest {
                traj,
                wake: Some(call.wake.clone()),
                title: str_arg(&call.args, "title")?,
                body: str_arg(&call.args, "body")?,
                from,
                supersedes: ids(&call.args, "supersedes"),
                at: chrono::Utc::now(),
            })
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!(
                "drafted requirement {} — Andrey accepts, edits or rejects it: {}",
                claim.claim, claim.title
            ),
            value: Some(serde_json::json!({ "claim": claim.claim.as_str() })),
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

struct NoteTimeline(LeaderHandle);

#[async_trait::async_trait]
impl Tool for NoteTimeline {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }
    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let at_text = str_arg(&call.args, "at")?;
        let at = chrono::DateTime::parse_from_rfc3339(&at_text)
            .map_err(|e| ToolFailure {
                kind: FailureClass::Error,
                message: format!("`at` must be an RFC3339 moment: {e}"),
            })?
            .with_timezone(&chrono::Utc);
        let step = self
            .0
            .note_timeline(TimelineEntry {
                title: str_arg(&call.args, "title")?,
                at,
                agents: strings(&call.args, "agents")
                    .into_iter()
                    .map(|s| AgentName::new(&s))
                    .collect(),
                refs: strings(&call.args, "refs")
                    .into_iter()
                    .map(|s| Ref::new(&s))
                    .collect(),
                cites: cites(&call.args),
            })
            .await
            .map_err(|e| failed(e.to_string()))?;
        Ok(ToolOutcome {
            content: format!("noted on the timeline ({step})"),
            value: Some(serde_json::json!({ "step": step.as_str() })),
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

fn ids(args: &serde_json::Value, key: &str) -> Vec<StepId> {
    strings(args, key).into_iter().map(StepId::new).collect()
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
    fn the_scoped_propose_claim_admits_a_structural_kind() {
        let kind = kind_of(
            &serde_json::json!({
                "kind": "lane",
                "detail": { "name": "infra", "routing_refs": ["repo:bough"] }
            }),
            false,
        )
        .expect("the leader may propose structure");
        assert!(kind.is_structural());
        assert_eq!(kind.as_str(), "lane");
    }

    #[test]
    fn it_still_admits_the_open_kinds() {
        let kind = kind_of(
            &serde_json::json!({ "kind": "requirement", "supersedes": ["p1"] }),
            false,
        )
        .expect("a requirement");
        assert_eq!(
            kind,
            ClaimKind::Requirement {
                supersedes: vec![StepId::new("p1")]
            }
        );
    }

    #[test]
    fn propose_structure_refuses_a_non_structural_kind() {
        let err = kind_of(&serde_json::json!({ "kind": "requirement" }), true)
            .expect_err("propose_structure is about structure");
        assert_eq!(err.kind, FailureClass::Denied);
        assert!(
            err.message.contains("not a structural kind"),
            "{}",
            err.message
        );
    }

    #[test]
    fn a_structural_kind_with_an_unreadable_detail_is_an_error_not_a_note() {
        let err = kind_of(
            &serde_json::json!({ "kind": "split", "detail": { "children": "nonsense" } }),
            true,
        )
        .expect_err("a split whose detail will not parse must not become `other`");
        assert!(err.message.contains("`detail`"), "{}", err.message);
    }

    #[test]
    fn cites_are_read_as_bare_refs() {
        let c = cites(&serde_json::json!({ "cites": ["step:s1", "gh:o/r#1"] }));
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].r#ref, Ref::new("step:s1"));
        assert!(c[0].url.is_none());
    }
}
