//! Invariant (§7): the natural path for a model IS the journalled path. Each of the four
//! primitives is a tool that calls `ActionsHandle::execute` and nothing else, so an act on the
//! world cannot happen without an intent row — and in Phase 2, with no Provider mounted, each
//! returns a refusal that names the kind.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_actions::{ActionError, ActionKind, ActionRequest, ActionTarget, Actions};
use bough_plugin_ledger::StepId;
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};
use chrono::{DateTime, Utc};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "tool-actions";

/// What every action tool takes: a target and a kind-specific payload.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ActionArgs {
    /// `owner/repo#12`, `TEAM-123`, a thread id — canonicalised by the seam, not here.
    pub target: String,
    #[serde(default)]
    pub payload: serde_json::Value,
}

/// One of the four primitives, as a tool.
///
/// It holds the seam rather than reading it from `cx.ctx`: the tool is registered by the row that
/// injected `actions`, and a tool must not be able to reach a service its row never declared.
/// The clock is injected for the same reason the ledger's is (AGENTS.md).
pub struct ActionTool {
    pub kind: ActionKind,
    pub actions: Arc<bough_plugin_actions::ActionsHandle>,
    pub now: fn() -> DateTime<Utc>,
}

#[async_trait::async_trait]
impl Tool for ActionTool {
    /// An outward act is never concurrency-safe: two of them in one step form a barrier, so the
    /// journal sees them one at a time (§9).
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }

    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let args: ActionArgs =
            serde_json::from_value(call.args.clone()).map_err(|e| ToolFailure {
                kind: FailureClass::Error,
                message: format!("`{}` needs {{target, payload}}: {e}", self.kind.as_str()),
            })?;
        let req = ActionRequest {
            kind: self.kind,
            target: ActionTarget::new(args.target),
            payload: args.payload,
            agent: call.agent.clone(),
            wake: call.wake.clone(),
            step: triggering_step(&call),
            at: (self.now)(),
        };
        match self.actions.execute(&_cx.ctx, req).await {
            Ok(artifact) => Ok(ToolOutcome {
                content: format!("{} done: {}", self.kind.as_str(), artifact.locator),
                value: Some(serde_json::json!({
                    "locator": artifact.locator,
                    "marker": artifact.marker,
                })),
                cites: vec![],
                concludes_wake: false,
            }),
            Err(e) => Err(failure(self.kind, e)),
        }
    }
}

/// The step the action is attributed to (§7's idem key).
///
/// DEVIATION from the plan's `ActionRequest.step`: §9's `ToolCall` carries no `StepId` — the
/// `tool/call` step is written by the tools pipeline and its id is not handed to the tool. The
/// wake plus the step index IS that step's coordinate, and it is what the idem key needs it for:
/// two wakes double-processing one piece of mail land on different coordinates, and one call
/// retried inside a step lands on the same one.
fn triggering_step(call: &ToolCall) -> StepId {
    StepId::new(format!("{}#{}", call.wake, call.step_index))
}

/// Every refusal NAMES THE KIND, because that is what the model has to reason about: `open_pr` is
/// a capability this harness does not have, not a malfunction of a tool called `open_pr`.
fn failure(kind: ActionKind, e: ActionError) -> ToolFailure {
    let class = match e {
        // A boundary refusal: the harness will not do it, and no retry changes that.
        ActionError::NoProvider(_) | ActionError::Duplicate { .. } => FailureClass::Denied,
        _ => FailureClass::Error,
    };
    ToolFailure {
        kind: class,
        message: format!("{}: {e}", kind.as_str()),
    }
}

/// The model-visible name of each kind. A fifth spelling is not a tool at all (§7).
pub fn tool_name(kind: ActionKind) -> &'static str {
    kind.as_str()
}

/// What the model is told each primitive does. The schema describes the primitive; the boundary
/// itself is code, in the seam (§7).
pub fn description(kind: ActionKind) -> &'static str {
    match kind {
        ActionKind::OpenPr => {
            "Open a pull request as Andrey. `target` is the repository (`owner/repo` or its url); \
             `payload` carries `{head, base, title, body}`. Journalled and idempotent: the same \
             PR from the same step is opened once."
        }
        ActionKind::PushToPr => {
            "Push commits to an OPEN pull request Andrey authored — never a teammate's branch. \
             `target` is `owner/repo#number` or the PR url; `payload` carries `{branch, commits}`."
        }
        ActionKind::BotThreadOp => {
            "Reply to, resolve or close a BOT review thread. `target` is `owner/repo#number`; \
             `payload` carries `{thread, op, body}`. Human threads are never auto-resolved."
        }
        ActionKind::LinearWrite => {
            "Change a Linear ticket's status or comment on it. `target` is the identifier \
             (`TEAM-123`) or its url; `payload` carries `{status}` or `{comment}`. Creating \
             tickets is Andrey's, and is not this tool."
        }
    }
}

/// The spec each primitive registers under.
pub fn spec(kind: ActionKind, actions: Arc<bough_plugin_actions::ActionsHandle>) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(tool_name(kind)),
        description: description(kind).to_string(),
        input_schema: schemars::schema_for!(ActionArgs),
        render: RenderIntent::Generic,
        scope: ToolScope::Global,
        tool: Arc::new(ActionTool {
            kind,
            actions,
            now: Utc::now,
        }),
    }
}

/// No configuration.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ToolActionsConfig {}

/// The consumer row: four tools, one per kind.
pub struct ToolActionsPlugin;

#[async_trait::async_trait]
impl Plugin for ToolActionsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ToolActionsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "actions"])
    }

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tools = ctx
            .get::<Tools>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let actions = ctx
            .get::<Actions>()
            .map_err(|e| PluginError::new(entry, e))?;
        // FOUR registrations, one per kind, each its own effect: §7's set is closed, so this loop
        // is the whole model-facing surface of the write boundary.
        for kind in ActionKind::all() {
            tools.register(&ctx, spec(*kind, actions.clone())).await?;
        }
        Ok(())
    }
}

bough_kernel::register_plugin!(ToolActionsPlugin);

/// Register one tool per action kind THAT HAS A LIVE PROVIDER (phase ux1 §2.10, M25).
/// `ActionsHandle::kinds()` already answers this and is "empty in Phase 2, on purpose" — this row
/// just stops ignoring it. With no `actions-github` row, `open_pr` and `push_to_pr` are absent
/// from the prompt entirely: §9's rule that a filtered-away tool is indistinguishable from one
/// that never existed.
///
/// Registrations are effects, so the set is RECONCILED, not registered once: the row re-reads
/// `kinds()` on its tick and disposes the tools whose kind withdrew. (There is no
/// `actions/provider-changed` event today and this phase does not add one — no Provider exists to
/// raise it before Phase 6. When `actions-github` lands, that event replaces the tick.)
///
/// Returns the tool names live after the reconcile.
pub fn reconcile_action_tools(
    actions: &bough_plugin_actions::ActionsHandle,
    tools: &bough_plugin_tools::ToolsHandle,
) -> Vec<ToolName> {
    let _ = (actions, tools);
    todo!("WP-7")
}
