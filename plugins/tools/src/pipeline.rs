//! Invariant (§9): the guard is MONOTONE BY TYPE. `Decision` has no public widening constructor —
//! a listener can call `deny()` or `ask()` and nothing else — so a denial cannot be re-allowed by
//! a later listener and monotonicity is a property of the types, not of a review (P2-D12).

use std::sync::Arc;
use std::time::Instant;

use bough_kernel::{EmitEvent, WaterfallEvent};
use bough_plugin_ledger::AgentName;
use tokio_util::sync::CancellationToken;

use crate::tool::{AttachedContext, ToolCall, ToolFailure, ToolOutcome, ToolResult};

/// The guard's verdict. Ordered: `Allow` < `Ask` < `Deny`, and only the executor can start it at
/// `Allow`.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    Allow,
    Ask { reason: String },
    Deny { reason: String },
}

/// The value of the `tools/pre-execute` waterfall.
#[derive(Clone, Debug)]
pub struct PreExecute {
    pub call: Arc<ToolCall>,
    /// Private on purpose: the only mutators tighten.
    pub(crate) decision: Decision,
    pub agent: AgentName,
}

impl PreExecute {
    /// The executor's starting value. WP-3.
    pub fn new(call: Arc<ToolCall>, agent: AgentName) -> PreExecute {
        PreExecute {
            call,
            decision: Decision::Allow,
            agent,
        }
    }
    /// What the chain has decided so far.
    pub fn decision(&self) -> &Decision {
        &self.decision
    }
    /// `Allow | Ask | Deny -> Deny`. WP-3.
    pub fn deny(&mut self, _reason: impl Into<String>) {
        todo!("WP-3: tighten to Deny, keeping the first denial's reason")
    }
    /// `Allow -> Ask`; a `Deny` stays denied. WP-3.
    pub fn ask(&mut self, _reason: impl Into<String>) {
        todo!("WP-3: tighten to Ask unless already denied")
    }
}

/// `tools/pre-execute` — allow | deny | ask (§9).
pub struct ToolsPreExecute;
impl WaterfallEvent for ToolsPreExecute {
    const NAME: &'static str = "tools/pre-execute";
    type Value = PreExecute;
}

/// The value of the `tools/execute` waterfall: around-dispatch.
///
/// A wrapper may replace ONLY the cancellation signal, and deadlines WRAP (`min`), never lengthen.
/// The executor compares `call.digest()` after the chain and ignores (and logs) any edit: §9 does
/// not offer input rewrite, and nothing in a waterfall enforces that on its own (P2-D13).
#[derive(Clone)]
pub struct Execution {
    pub call: Arc<ToolCall>,
    pub cancel: CancellationToken,
    pub deadline: Option<Instant>,
    pub outcome: Option<Result<ToolOutcome, ToolFailure>>,
}

/// `tools/execute`.
pub struct ToolsExecute;
impl WaterfallEvent for ToolsExecute {
    const NAME: &'static str = "tools/execute";
    type Value = Execution;
}

/// The value of the `tools/post-execute` waterfall.
#[derive(Clone, Debug)]
pub struct PostExecute {
    pub call: Arc<ToolCall>,
    /// Private: the only mutators are the four below, which is how "content OR value, never
    /// both" is kept true.
    pub(crate) result: ToolResult,
}

impl PostExecute {
    /// The executor's starting value. WP-3.
    pub fn new(call: Arc<ToolCall>, result: ToolResult) -> PostExecute {
        PostExecute { call, result }
    }
    /// The result as the chain has it so far.
    pub fn result(&self) -> &ToolResult {
        &self.result
    }
    /// Replace the content, CLEARING the value. WP-3.
    pub fn accept_content(&mut self, _content: String) {
        todo!("WP-3: set content, clear value")
    }
    /// Replace the value, CLEARING the content. WP-3.
    pub fn accept_value(&mut self, _value: serde_json::Value) {
        todo!("WP-3: set value, clear content")
    }
    /// Attach a context without touching content or value. WP-3.
    pub fn attach(&mut self, _ctx: AttachedContext) {
        todo!("WP-3: push onto attached")
    }
    /// Turn the result into a VALUELESS failure with [`crate::FailureClass::Blocked`]. WP-3.
    pub fn block(&mut self, _reason: impl Into<String>) {
        todo!("WP-3: blocked failure, no value, no content")
    }
}

/// `tools/post-execute`.
pub struct ToolsPostExecute;
impl WaterfallEvent for ToolsPostExecute {
    const NAME: &'static str = "tools/post-execute";
    type Value = PostExecute;
}

/// `tools/result` — emit, observe-only, immutable (§9).
pub struct ToolsResult;
impl EmitEvent for ToolsResult {
    const NAME: &'static str = "tools/result";
    type Payload = Arc<ToolResult>;
}
