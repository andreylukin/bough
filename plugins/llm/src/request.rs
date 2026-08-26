//! Invariant: `agent/request` listeners can write the CALL CONFIG and nothing else (§5, §12).
//! The facts are behind an `Arc` and the loop re-installs its own copy after the chain, so
//! "cannot mutate the messages" is a property of the types rather than of a review (P2-D4).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bough_kernel::WaterfallEvent;
use bough_plugin_ledger::{AgentName, TrajId, WakeId};

pub use bough_llm::types::{Effort, LlmContentBlock, LlmMessage, LlmRole, LlmToolDef};

use crate::stream::LlmFailure;

/// One model request, exactly as the adapter will send it.
#[derive(Clone, Debug, PartialEq)]
pub struct LlmRequest {
    pub model: String,
    /// The STABLE prefix (bough-llm's cache contract). The loop puts the projection here.
    pub system: Option<String>,
    pub system_volatile: Option<String>,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<LlmToolDef>,
    pub call: CallConfig,
}

impl LlmRequest {
    /// Canonical JSON of the whole request: stable field order, no clock, no ids minted here.
    /// The unit of V4's byte-for-byte comparison and of `request/header`'s `projection_digest`.
    ///
    /// WP-1.
    pub fn canonical(&self) -> String {
        todo!("WP-1: canonical JSON of the request, stable field order")
    }

    /// sha256 of [`LlmRequest::canonical`], hex.
    ///
    /// WP-1.
    pub fn digest(&self) -> String {
        todo!("WP-1: sha256 of canonical()")
    }
}

/// The ONLY thing an `agent/request` listener may write.
#[derive(Clone, Debug, PartialEq)]
pub struct CallConfig {
    pub model: String,
    pub max_tokens: i64,
    pub effort: Option<Effort>,
    /// `true` forbids tool use for this call — the grace step sets it (§5).
    pub tool_choice_none: bool,
    /// Metering, budget notes, anything a listener carries to the next listener.
    pub meta: BTreeMap<String, serde_json::Value>,
}

/// Read-only facts a policy listener needs.
#[derive(Clone, Debug, PartialEq)]
pub struct RequestFacts {
    pub agent: AgentName,
    pub traj: TrajId,
    pub wake: WakeId,
    pub wake_kind: WakeKind,
    pub step_index: u32,
    /// The one predicate §12's model policy turns on.
    pub answers_andrey: bool,
    /// `agents.model_override`, read from the ledger row. Unattended wakes only (§12).
    pub model_override: Option<String>,
    pub prompt_ver: String,
    /// The composition fingerprint (§0.5).
    pub composition: String,
}

/// Why this wake is running.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum WakeKind {
    /// Answers Andrey. Always sol (§12).
    Answer,
    /// A debounced drain of ordinary mail.
    Drain,
    Scheduled,
    Catchup,
    /// A worker's own task wake.
    Task,
}

/// The value of the `agent/request` waterfall.
#[derive(Clone, Debug)]
pub struct RequestCall {
    pub facts: Arc<RequestFacts>,
    pub call: CallConfig,
}

/// §5/§12: a waterfall over the CALL CONFIG only. Re-exported from `bough-plugin-agents`.
pub struct AgentRequest;

impl WaterfallEvent for AgentRequest {
    const NAME: &'static str = "agent/request";
    type Value = RequestCall;
}

/// The value of the `agent/request-error` waterfall.
#[derive(Clone, Debug)]
pub struct RequestErrorCall {
    pub facts: Arc<RequestFacts>,
    pub request: Arc<LlmRequest>,
    pub failure: LlmFailure,
    /// 1 for the first failure of this step.
    pub attempt: u32,
    pub recovery: Recovery,
}

/// What the chain decided about a failed request.
#[derive(Clone, Debug, PartialEq)]
pub enum Recovery {
    /// The default: the failure stands and the wake ends with reason `error`.
    Terminal,
    /// Re-enter the request after `after`, optionally with a rewritten request.
    Retry {
        after: Duration,
        request: Option<Arc<LlmRequest>>,
    },
}

/// §5: a listener that owns recovery returns `Recovery::Retry(..)` WITHOUT calling `next()`.
pub struct AgentRequestError;

impl WaterfallEvent for AgentRequestError {
    const NAME: &'static str = "agent/request-error";
    type Value = RequestErrorCall;
}
