//! The provider-neutral wire types: what a caller hands `LlmClient::run` and
//! what it gets back. No provider name appears here — each provider maps
//! these to its own encoding inside its own client.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::error::LlmError;

/// Per-round provider usage. Summed across a turn by the host; `cost_usd` is
/// stamped by `pricing::with_pricing` from the vendored catalog.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_tokens: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Thinking depth. Not every model accepts one; an unsupported value is a turn
/// error.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Effort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

/// A content block as the model produces it. `meta` on `Reasoning` is an
/// opaque provider payload replayed VERBATIM — never inspected outside the
/// provider's own mapper. Note the wire asymmetry vs persisted parts:
/// `toolUseId`/`content` here, `callId`/`output` there. Two types, never
/// unified.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LlmBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
}

/// A block as it appears in a request message.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(
    tag = "type",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum LlmContentBlock {
    Text {
        text: String,
    },
    Reasoning {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<Value>,
    },
    ToolUse {
        id: String,
        name: String,
        #[serde(default)]
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        is_error: bool,
    },
    /// Base64-encoded at assembly time; each provider maps it to its native shape.
    Image {
        data: String,
        media_type: String,
        name: String,
    },
}

impl From<LlmBlock> for LlmContentBlock {
    fn from(b: LlmBlock) -> Self {
        match b {
            LlmBlock::Text { text } => LlmContentBlock::Text { text },
            LlmBlock::Reasoning { text, meta } => LlmContentBlock::Reasoning { text, meta },
            LlmBlock::ToolUse { id, name, input } => LlmContentBlock::ToolUse { id, name, input },
        }
    }
}

#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LlmRole {
    User,
    Assistant,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct LlmMessage {
    pub role: LlmRole,
    pub content: Vec<LlmContentBlock>,
}

/// The model sees exactly two of these: `run_steps` and `stop`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LlmToolDef {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LlmParams {
    pub model: String,
    /// The STABLE system prefix. Prompt-cache contract: byte-identical across
    /// sessions and turns per delegation tier.
    pub system: Option<String>,
    /// The per-session suffix, sent after `system` with its own cache
    /// breakpoint.
    pub system_volatile: Option<String>,
    pub max_tokens: i64,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<LlmToolDef>,
    /// `Some(ToolChoiceNone)` forbids tool calls for this round, forcing plain
    /// text — the runner's last resort.
    pub tool_choice_none: bool,
    pub effort: Option<Effort>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LlmResult {
    pub content: Vec<LlmBlock>,
    pub stop_reason: String,
    pub usage: Option<Usage>,
}

/// Streamed text deltas as they arrive.
pub type OnText = Arc<dyn Fn(&str) + Send + Sync>;

/// The whole provider surface. The turn runner must not know which provider it
/// is talking to — if provider-specific handling leaks past this interface, it
/// leaks everywhere.
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// One round. Cancelling `cancel` aborts the in-flight request; the caller
    /// treats the resulting abort as an interrupt.
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        cancel: CancellationToken,
    ) -> Result<LlmResult, LlmError>;
}
