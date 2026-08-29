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
    /// The STABLE prefix (bough-llm's cache contract). The loop puts the projection's stable
    /// tier here: identity, pins, digest, tier summaries.
    pub system: Option<String>,
    /// The VOLATILE suffix, sent after `system` with its own cache breakpoint. The loop puts the
    /// projection's tail band and mail here: the sections that move every wake, kept out of
    /// `system` so the provider's cache can re-read the stable tier across wakes.
    pub system_volatile: Option<String>,
    pub messages: Vec<LlmMessage>,
    pub tools: Vec<LlmToolDef>,
    pub call: CallConfig,
    /// `request/header.projection_digest`, riding along so a listener (the request recorder) can
    /// key its record by the SAME digest the header carries without recombining the tiers.
    /// Seam-internal: never on the wire, and deliberately NOT in [`LlmRequest::canonical`] — it
    /// is derived from the fields that already are. Empty when no projection built this request
    /// (the governance and summarizer callers).
    pub projection_digest: Option<String>,
}

impl LlmRequest {
    /// Canonical JSON of the whole request: stable field order, no clock, no ids minted here.
    /// The unit of V4's byte-for-byte comparison and of `request/header`'s `projection_digest`.
    ///
    /// The field order is the DECLARATION order of the JSON object built here — not `Debug`, not
    /// a derived `Serialize` — so a field added later cannot silently reorder every past digest.
    pub fn canonical(&self) -> String {
        let call = serde_json::json!({
            "model": self.call.model,
            "max_tokens": self.call.max_tokens,
            "effort": self.call.effort,
            "tool_choice_none": self.call.tool_choice_none,
            // A `BTreeMap` serialises in key order, so a listener's insertion order is invisible.
            "meta": self.call.meta,
        });
        let value = serde_json::json!({
            "model": self.model,
            "system": self.system,
            "system_volatile": self.system_volatile,
            "messages": self.messages,
            "tools": self.tools,
            "call": call,
        });
        serde_json::to_string(&value).expect("an LlmRequest is JSON by construction")
    }

    /// sha256 of [`LlmRequest::canonical`], hex.
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        h.update(self.canonical().as_bytes());
        format!("{:x}", h.finalize())
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
    /// Re-enter the request after `after`.
    ///
    /// There is deliberately NO "and here is a rewritten request" field. Everything the model
    /// sees is rebuilt from the ledger (§0.2, P2-D19), so a repair that changes what the model
    /// sees — §5's overflow repair — repairs the LEDGER-visible inputs (the projection budget,
    /// a rollup) and lets the loop rebuild; a request handed sideways to the adapter would be a
    /// side channel and V4 would report it. The field used to exist and no consumer could ever
    /// have been honoured, which is worse than not offering it.
    Retry { after: Duration },
}

/// §5: a listener that owns recovery returns `Recovery::Retry(..)` WITHOUT calling `next()`.
pub struct AgentRequestError;

impl WaterfallEvent for AgentRequestError {
    const NAME: &'static str = "agent/request-error";
    type Value = RequestErrorCall;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> LlmRequest {
        LlmRequest {
            projection_digest: None,
            model: "claude-haiku-4-5-20251001".into(),
            system: Some("stable".into()),
            system_volatile: None,
            messages: vec![LlmMessage {
                role: LlmRole::User,
                content: vec![LlmContentBlock::Text { text: "hi".into() }],
            }],
            tools: vec![],
            call: CallConfig {
                model: "claude-haiku-4-5-20251001".into(),
                max_tokens: 1024,
                effort: None,
                tool_choice_none: false,
                meta: BTreeMap::new(),
            },
        }
    }

    /// V4 compares requests byte for byte, so the canonical form must not depend on the order a
    /// listener happened to write its metering keys in.
    #[test]
    fn meta_key_order_does_not_move_the_digest() {
        let mut a = req();
        a.call.meta.insert("z".into(), serde_json::json!(1));
        a.call.meta.insert("a".into(), serde_json::json!(2));
        let mut b = req();
        b.call.meta.insert("a".into(), serde_json::json!(2));
        b.call.meta.insert("z".into(), serde_json::json!(1));
        assert_eq!(a.canonical(), b.canonical());
        assert_eq!(a.digest(), b.digest());
    }

    #[test]
    fn a_changed_field_changes_the_digest() {
        let a = req();
        let mut b = req();
        b.call.max_tokens = 2048;
        assert_ne!(a.digest(), b.digest());
        assert_eq!(a.digest(), req().digest(), "and it is stable across calls");
    }
}
