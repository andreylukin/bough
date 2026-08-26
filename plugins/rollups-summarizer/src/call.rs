//! Invariant (P4-D3): this is the ONE place a governance row reaches a model, and it reaches it
//! the way the loop does — through the `agent/request` waterfall. `answers_andrey` is FALSE and
//! `wake_kind` is `Scheduled`, so `model-policy` chooses terra and an agent's `model_override`
//! applies exactly as §12 says it does for unattended work. Nothing in this module names a model.

use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_ledger::{AgentName, LedgerHandle, SeqRange, TrajId};
use bough_plugin_llm::{LlmHandle, RequestFacts};
use bough_plugin_rollups::{PassId, RollupsError};

use crate::SummarizerConfig;

/// Which half of the map/reduce a call is.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    Map,
    Reduce,
    Digest,
}

/// Where a call's token counts came from. A provider that reported none is SAID to be estimated
/// rather than presented as measured (§16).
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum TokenSource {
    Provider,
    Estimate,
}

/// One model call's request.
pub struct CallRequest {
    pub phase: Phase,
    pub facts: Arc<RequestFacts>,
    /// The versioned recap prompt.
    pub system: String,
    /// The rendered window, or the rendered children.
    pub user: String,
    pub max_tokens: i64,
    pub tier: u8,
    pub range: SeqRange,
}

/// One model call's answer, with what it cost.
pub struct CallOutcome {
    pub text: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub source: TokenSource,
}

/// Build the read-only facts for a governance call.
pub fn facts(
    _agent: &AgentName,
    _traj: &TrajId,
    _pass: &PassId,
    _cfg: &SummarizerConfig,
    _composition: &str,
) -> RequestFacts {
    todo!("WP-2: governance RequestFacts")
}

/// One model call: run `agent/request` for the call config, `llm.stream` for the answer, append
/// the `rollup/request` step, return the assembled text and the token counts.
pub async fn call(
    _ctx: &Context,
    _llm: &LlmHandle,
    _ledger: &LedgerHandle,
    _req: CallRequest,
) -> Result<CallOutcome, RollupsError> {
    todo!("WP-2: the governance model call")
}
