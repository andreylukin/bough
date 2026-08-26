//! Invariant: `draft_message` and `draft_ticket` are the ONLY outward-shaped tools the model is
//! ever shown besides the four action primitives, and neither can send. Their descriptions say so
//! in the model's own terms, because the instructional boundary is only as good as the sentence the
//! model actually reads (§7).

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};

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

    /// Register both tools on `ctx.tools`, `RenderIntent::Generic`, concurrency-safe. WP-4.
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-4")
    }

    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}
