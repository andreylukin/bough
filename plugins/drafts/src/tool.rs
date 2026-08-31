//! Invariant: `draft_message` and `draft_ticket` are the ONLY outward-shaped tools the model is
//! ever shown besides the four action primitives, and neither can send. Their descriptions say so
//! in the model's own terms, because the instructional boundary is only as good as the sentence the
//! model actually reads (§7).

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_tools::{
    FailureClass, RenderIntent, Tool, ToolCall, ToolCx, ToolFailure, ToolName, ToolOutcome,
    ToolScope, ToolSpec, Tools,
};

use crate::{DraftError, DraftKind, Drafts, DraftsHandle, NewDraft};

/// `draft_message`'s arguments.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftMessageArgs {
    /// Where it would go: `slack:#eng`, `email:someone`.
    pub audience: String,
    pub subject: String,
    pub body: String,
}

/// `draft_ticket`'s arguments.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftTicketArgs {
    pub audience: String,
    pub title: String,
    pub body: String,
}

/// What the model reads for `draft_message`.
pub const DRAFT_MESSAGE_DESCRIPTION: &str = "Write a message you are NOT sending. Use this for \
every Slack message, DM or email. Andrey reads it in the drafts pane and sends it or does not.";

/// What the model reads for `draft_ticket`.
pub const DRAFT_TICKET_DESCRIPTION: &str =
    "Write a ticket you are NOT creating. Creating tickets is Andrey's.";

/// The tool names, spelled once. The pane's script and the probe both name them.
pub const DRAFT_MESSAGE_TOOL: &str = "draft_message";
/// `draft_ticket`.
pub const DRAFT_TICKET_TOOL: &str = "draft_ticket";

/// One of the two tools. Both do the same thing to different vocabulary, so they are one type
/// with a kind: a second implementation is a second place for a send to appear.
pub struct DraftTool {
    kind: DraftKind,
    drafts: DraftsHandle,
}

impl DraftTool {
    /// The registration for one kind, over the handle this row injected. The handle is CAPTURED
    /// rather than resolved from `ToolCx::ctx`: a tool that reaches into the calling loop's
    /// context sees whatever that context can see (§0.3).
    pub fn spec(kind: DraftKind, drafts: DraftsHandle) -> ToolSpec {
        let (name, description, schema) = match kind {
            DraftKind::Message => (
                DRAFT_MESSAGE_TOOL,
                DRAFT_MESSAGE_DESCRIPTION,
                schemars::SchemaGenerator::default().into_root_schema_for::<DraftMessageArgs>(),
            ),
            DraftKind::Ticket => (
                DRAFT_TICKET_TOOL,
                DRAFT_TICKET_DESCRIPTION,
                schemars::SchemaGenerator::default().into_root_schema_for::<DraftTicketArgs>(),
            ),
        };
        ToolSpec {
            name: ToolName::new(name),
            description: description.to_string(),
            input_schema: schema,
            render: RenderIntent::Generic,
            scope: ToolScope::Global,
            tool: Arc::new(DraftTool { kind, drafts }),
        }
    }
}

#[async_trait::async_trait]
impl Tool for DraftTool {
    /// One append and nothing else: two drafts at once cannot interfere.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        true
    }

    async fn call(&self, call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        let bad = |e: String| ToolFailure {
            kind: FailureClass::Error,
            message: e,
        };
        let (audience, subject, body) = match self.kind {
            DraftKind::Message => {
                let a: DraftMessageArgs = serde_json::from_value(call.args.clone())
                    .map_err(|e| bad(format!("bad arguments for `{}`: {e}", call.name)))?;
                (a.audience, a.subject, a.body)
            }
            DraftKind::Ticket => {
                let a: DraftTicketArgs = serde_json::from_value(call.args.clone())
                    .map_err(|e| bad(format!("bad arguments for `{}`: {e}", call.name)))?;
                (a.audience, a.title, a.body)
            }
        };
        let row = self
            .drafts
            .draft(NewDraft {
                kind: self.kind,
                agent: call.agent.clone(),
                wake: call.wake.clone(),
                audience,
                subject,
                body,
                refs: Default::default(),
                at: chrono::Utc::now(),
            })
            .await
            .map_err(|e| match e {
                // A refusal the model can fix by calling again is `Denied`, not `Error`: it is
                // being told what a draft needs, not that the harness broke.
                DraftError::NoAudience | DraftError::Empty => ToolFailure {
                    kind: FailureClass::Denied,
                    message: e.to_string(),
                },
                other => bad(other.to_string()),
            })?;
        // The model is told, in its own terms, that the act is FINISHED and nothing was sent.
        let what = match self.kind {
            DraftKind::Message => "message",
            DraftKind::Ticket => "ticket",
        };
        Ok(ToolOutcome {
            content: format!(
                "drafted {what} `{}` for {} — NOT sent. Andrey reads it in the drafts pane. This                  is the finished act for you: do not look for another way to deliver it.",
                row.id, row.audience
            ),
            value: Some(serde_json::json!({ "draft": row.id.as_str(), "sent": false })),
            cites: Vec::new(),
            concludes_wake: false,
        })
    }
}

/// No configuration: the two tools are §7's, not a deployment's.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DraftToolsConfig {}

/// The tool row.
pub struct DraftToolsPlugin;

#[async_trait::async_trait]
impl Plugin for DraftToolsPlugin {
    const NAME: &'static str = crate::TOOL_PLUGIN_NAME;
    type Config = DraftToolsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["tools", "drafts"])
    }

    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    /// Register both tools on `ctx.tools`. A REGISTRATION IS AN EFFECT: unloading this row takes
    /// both tools away and leaves no way to write a draft — and still no way to send one.
    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let entry = ctx.entry_id().clone();
        let tools = ctx
            .get::<Tools>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        let drafts = ctx
            .get::<Drafts>()
            .map_err(|e| PluginError::new(entry, e))?;
        for kind in [DraftKind::Message, DraftKind::Ticket] {
            tools
                .register(&ctx, DraftTool::spec(kind, (*drafts).clone()))
                .await?;
        }
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}
