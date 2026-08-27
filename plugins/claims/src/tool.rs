//! Invariant (§2): this is the GLOBAL `propose_claim` — any lane agent may propose a
//! `Requirement`, a `Contradiction` or `Other`, and a STRUCTURAL kind is refused with the reason
//! "only the leader proposes structure". Its leader-scoped twin lives in `tool-leader` and accepts
//! the structural kinds; that difference is V6's shadowing subject and a real behavioural
//! difference rather than a test fixture.

use std::sync::Arc;

use bough_kernel::{Context, PluginError};
use bough_plugin_ledger::StepId;
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};

use crate::{kind, ClaimKind, ClaimsHandle, ProposeRequest};

/// The tool's name, in both this crate and `tool-leader`.
pub const TOOL_NAME: &str = "propose_claim";

/// The kinds this tool admits, in the words the model reads.
pub const GLOBAL_KINDS: [&str; 3] = ["requirement", "contradiction", "other"];

/// Register the global tool, if `tools` is bound.
pub async fn register(ctx: &Context, claims: &ClaimsHandle) -> Result<(), PluginError> {
    // ABSENT is headless: the seam works with no model-facing surface at all. An ERROR is the
    // kernel refusing the read and is a boot failure (§0.2).
    let tools = match ctx.try_get::<Tools>() {
        Ok(Some(t)) => t,
        Ok(None) => return Ok(()),
        Err(e) => return Err(PluginError::new(ctx.entry_id().clone(), e)),
    };
    tools
        .register(
            ctx,
            ToolSpec {
                name: ToolName::new(TOOL_NAME),
                description: "Propose something you cannot make true on your own: a requirement, \
                              a contradiction between two steps, or a free note. Andrey accepts, \
                              edits or rejects it."
                    .to_string(),
                input_schema: schema(),
                render: RenderIntent::Generic,
                scope: ToolScope::Global,
                tool: Arc::new(ProposeClaim(claims.clone())),
            },
        )
        .await?;
    Ok(())
}

fn schema() -> schemars::Schema {
    schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "properties": {
            "kind": { "type": "string", "enum": GLOBAL_KINDS },
            "title": { "type": "string" },
            "body": { "type": "string" },
            "supersedes": { "type": "array", "items": { "type": "string" } },
            "between": { "type": "array", "items": { "type": "string" } }
        },
        "required": ["kind", "title", "body"]
    }))
    .expect("a literal object schema")
}

struct ProposeClaim(ClaimsHandle);

/// PURE: the claim kind a call asks for, refusing structure (§2) before anything is written.
pub fn kind_of(args: &serde_json::Value) -> Result<ClaimKind, ToolFailure> {
    let name = args.get("kind").and_then(|v| v.as_str()).unwrap_or("other");
    let ids = |key: &str| -> Vec<StepId> {
        args.get(key)
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str())
                    .map(StepId::new)
                    .collect()
            })
            .unwrap_or_default()
    };
    // The refusal is by NAME, before the parse: a structural kind whose detail this binary
    // cannot read is still a structural kind (§2).
    kind::refuse_structural_name(name).map_err(|e| ToolFailure {
        kind: FailureClass::Denied,
        message: e.to_string(),
    })?;
    let kind = match name {
        "requirement" => ClaimKind::Requirement {
            supersedes: ids("supersedes"),
        },
        "contradiction" => ClaimKind::Contradiction {
            between: ids("between"),
        },
        "other" => ClaimKind::Other,
        // A structural kind is REFUSED rather than downgraded to `Other`: an agent that asked to
        // split a lane must be told it may not, not quietly given a note.
        other => kind::parse(other, &serde_json::json!({})),
    };
    kind::refuse_structure_from_a_lane(&kind).map_err(|e| ToolFailure {
        kind: FailureClass::Denied,
        message: e.to_string(),
    })?;
    Ok(kind)
}

#[async_trait::async_trait]
impl Tool for ProposeClaim {
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let kind = kind_of(&call.args)?;
        let title = str_arg(&call.args, "title")?;
        let body = str_arg(&call.args, "body")?;
        let row = self
            .0
             .0
            .ledger
            .0
            .agent(&call.agent)
            .await
            .map_err(|e| failed(e.to_string()))?
            .ok_or_else(|| failed(format!("no agent row for `{}`", call.agent)))?;
        let claim = self
            .0
            .propose(ProposeRequest {
                by: call.agent.clone(),
                traj: row.traj,
                wake: Some(call.wake.clone()),
                kind,
                title,
                body,
                cites: Vec::new(),
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
    fn the_global_tool_refuses_a_structural_kind() {
        let err = kind_of(&serde_json::json!({ "kind": "lane", "name": "infra" }))
            .expect_err("only the leader proposes structure");
        assert_eq!(err.kind, FailureClass::Denied);
        assert!(
            err.message.contains("only the leader proposes structure"),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_global_tool_admits_the_three_open_kinds() {
        assert_eq!(
            kind_of(&serde_json::json!({ "kind": "requirement", "supersedes": ["p1"] }))
                .expect("a requirement"),
            ClaimKind::Requirement {
                supersedes: vec![StepId::new("p1")]
            }
        );
        assert_eq!(
            kind_of(&serde_json::json!({ "kind": "contradiction", "between": ["s1", "s2"] }))
                .expect("a contradiction"),
            ClaimKind::Contradiction {
                between: vec![StepId::new("s1"), StepId::new("s2")]
            }
        );
        assert_eq!(
            kind_of(&serde_json::json!({ "kind": "wat" })).expect("an unknown kind is harmless"),
            ClaimKind::Other
        );
    }
}
