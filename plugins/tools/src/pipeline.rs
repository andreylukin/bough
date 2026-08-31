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
    /// `Allow | Ask | Deny -> Deny`. The FIRST denial's reason is what the model is told: a
    /// later listener can restate the refusal but cannot rewrite why the call was refused.
    pub fn deny(&mut self, reason: impl Into<String>) {
        if matches!(self.decision, Decision::Deny { .. }) {
            return;
        }
        self.decision = Decision::Deny {
            reason: reason.into(),
        };
    }
    /// `Allow -> Ask`; a `Deny` stays denied, and a second `ask` keeps the first reason.
    pub fn ask(&mut self, reason: impl Into<String>) {
        if matches!(self.decision, Decision::Allow) {
            self.decision = Decision::Ask {
                reason: reason.into(),
            };
        }
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
    /// Replace the content, CLEARING the value (§9: content OR value, never both).
    pub fn accept_content(&mut self, content: String) {
        self.result.content = content;
        self.result.value = None;
    }
    /// Replace the value, CLEARING the content (§9: content OR value, never both).
    pub fn accept_value(&mut self, value: serde_json::Value) {
        self.result.value = Some(value);
        self.result.content = String::new();
    }
    /// Attach a context without touching content or value.
    pub fn attach(&mut self, ctx: AttachedContext) {
        self.result.attached.push(ctx);
    }
    /// Turn the result into a VALUELESS failure with [`crate::FailureClass::Blocked`]. The
    /// feedback becomes the failure message the model sees, and the value is dropped: a blocked
    /// call must not leave a usable result behind.
    pub fn block(&mut self, reason: impl Into<String>) {
        let reason = reason.into();
        self.result.ok = false;
        self.result.value = None;
        self.result.content = reason.clone();
        self.result.concludes_wake = false;
        self.result.failure = Some(ToolFailure {
            kind: crate::tool::FailureClass::Blocked,
            message: reason,
        });
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
