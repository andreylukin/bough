//! Invariant: both tools register at `ToolScope::Agent(target)` where `target` comes from
//! `ctx.leader.target()` and NEVER from this row's own config (P5-D10). Two rows with two
//! spellings of one target is a misconfiguration that would present as "half the leader set
//! moved"; injecting the key makes the move atomic.
//!
//! WP-6 COLLAPSE: five tools became two. `propose_claim` SHADOWS the global one from `claims`
//! (it accepts the structural kinds the global one refuses — V6's shadowing subject) and it
//! ABSORBS `draft_requirement` (kind `requirement`, whose `cites` are required and enforced) and
//! `propose_structure` (the structural kinds). `curate` is `adopt_unsorted` + `note_timeline` in
//! one journalled pass. `plugins/leader` is untouched: this is a Consumer-side collapse only.
//!
//! And both of them PROPOSE or CURATE. A structural kind writes `claim/proposed`, never an op:
//! there is no path from this crate to `ctx.graph`.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_claims::{kind, ClaimKind, ClaimsHandle, ProposeRequest};
use bough_plugin_leader::{AdoptRequest, DraftRequest, LeaderHandle, TimelineEntry};
use bough_plugin_ledger::{AgentName, Cite, Ref, StepId};
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};

/// The two tool names, in registration order.
pub const TOOL_NAMES: [&str; 2] = ["propose_claim", "curate"];

/// The kinds the SCOPED `propose_claim` admits — the global three plus the structural four.
///
/// `contradiction` and `other` stay: the GLOBAL `propose_claim` admits them, and this spec
/// SHADOWS the global one for the leader's agent. Dropping them here would take capability away
/// from the leader that every ordinary lane keeps.
pub const LEADER_KINDS: [&str; 7] = [
    "requirement",
    "contradiction",
    "other",
    "lane",
    "split",
    "merge",
    "bud",
];

/// The structural four. `lane` is one: a new lane IS structure.
pub const STRUCTURAL_KINDS: [&str; 4] = ["lane", "split", "merge", "bud"];

/// Register both for the leader's target.
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

/// The two specs, both scoped to `target`.
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
             split, merge, bud — which an ordinary lane may not, and `requirement` writes down a \
             requirement you heard from Andrey (cite his words in `cites`). This proposes; it \
             never performs. Andrey decides.",
            serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": { "type": "string", "enum": LEADER_KINDS },
                    "title": { "type": "string" },
                    "body": { "type": "string" },
                    "detail": { "type": "object" },
                    "cites": { "type": "array", "items": { "type": "string" } },
                    "supersedes": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["kind", "title", "body"]
            }),
            Arc::new(ProposeClaim {
                claims: claims.clone(),
                leader: leader.clone(),
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

// ---- propose_claim ----------------------------------------------------------------------------

/// The shadowing `propose_claim`: the global three kinds, the structural four, and the
/// requirement-drafting path that used to be `draft_requirement`.
struct ProposeClaim {
    claims: ClaimsHandle,
    /// The trajectory a proposal lands on comes from the BINDING's target, not from `call.agent`:
    /// these tools are in the target's scope and nobody else's, so the two are the same name and
    /// reading it from the binding needs no second ledger dependency.
    leader: LeaderHandle,
}

/// PURE: the kind a call asks for. UNLIKE the global tool, a structural name is admitted.
///
/// A structural NAME whose `detail` this binary cannot read is an ERROR, never a silent `other`:
/// a `split` that degraded into a note would look accepted and do nothing.
pub fn kind_of(args: &serde_json::Value) -> Result<ClaimKind, ToolFailure> {
    let name = args.get("kind").and_then(|v| v.as_str()).unwrap_or("other");
    // The structured half rides `detail`, which is exactly where `claims::kind::parse` reads it
    // from a stored body — so a kind proposed here reads back as the same kind.
    let detail = match args.get("detail") {
        Some(d @ serde_json::Value::Object(_)) => d.clone(),
        _ => args.clone(),
    };
    let parsed = kind::parse(name, &serde_json::json!({ kind::DETAIL_KEY: detail }));
    if kind::is_structural_name(name) && !parsed.is_structural() {
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
        let kind = kind_of(&call.args)?;
        let traj = self
            .leader
            .traj()
            .await
            .map_err(|e| failed(e.to_string()))?;
        // A requirement is the ABSORBED `draft_requirement`: same path through `ctx.leader`, same
        // enforced cites, so the drafting behaviour did not move into this crate.
        if let ClaimKind::Requirement { .. } = kind {
            let from = cites(&call.args);
            if from.is_empty() {
                return Err(ToolFailure {
                    kind: FailureClass::Error,
                    message: "a requirement drafted from Andrey's words must cite them".to_string(),
                });
            }
            let claim = self
                .leader
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
            return Ok(ToolOutcome {
                content: format!(
                    "drafted requirement {} — Andrey accepts, edits or rejects it: {}",
                    claim.claim, claim.title
                ),
                value: Some(serde_json::json!({ "claim": claim.claim.as_str() })),
                cites: Vec::new(),
                concludes_wake: false,
            });
        }
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
        let kind = kind_of(&serde_json::json!({
            "kind": "lane",
            "detail": { "name": "infra", "routing_refs": ["repo:bough"] }
        }))
        .expect("the leader may propose structure");
        assert!(kind.is_structural());
        assert_eq!(kind.as_str(), "lane");
    }

    #[test]
    fn it_still_admits_the_open_kinds() {
        let kind = kind_of(&serde_json::json!({ "kind": "requirement", "supersedes": ["p1"] }))
            .expect("a requirement");
        assert_eq!(
            kind,
            ClaimKind::Requirement {
                supersedes: vec![StepId::new("p1")]
            }
        );
        // …and `contradiction`/`other` are still admitted, because the global tool admits them
        // and this spec SHADOWS it.
        assert_eq!(
            kind_of(&serde_json::json!({ "kind": "other" })).expect("other"),
            ClaimKind::Other
        );
        assert_eq!(
            kind_of(&serde_json::json!({ "kind": "contradiction" }))
                .expect("contradiction")
                .as_str(),
            "contradiction"
        );
    }

    #[test]
    fn a_structural_kind_with_an_unreadable_detail_is_an_error_not_a_note() {
        let err = kind_of(&serde_json::json!({
            "kind": "split", "detail": { "children": "nonsense" }
        }))
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
        assert_eq!(req.timeline[0].cites[0].r#ref, Ref::new("step:s1"));
    }

    #[test]
    fn a_timeline_entry_with_an_unreadable_moment_is_refused() {
        let err = curate_request(&serde_json::json!({
            "timeline": [{ "title": "t", "at": "yesterday", "cites": ["step:s1"] }]
        }))
        .expect_err("`at` is a moment");
        assert!(err.message.contains("RFC3339"), "{}", err.message);
    }
}
