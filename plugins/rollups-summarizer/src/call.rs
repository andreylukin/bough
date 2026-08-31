//! Invariant (P4-D3): this is the ONE place a governance row reaches a model, and it reaches it
//! the way the loop does — through the `agent/request` waterfall. `answers_andrey` is FALSE and
//! `wake_kind` is `Scheduled`, so `model-policy` chooses terra and an agent's `model_override`
//! applies exactly as §12 says it does for unattended work. Nothing in this module names a model.

use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_ledger::{AgentName, Cite, Ref};
use bough_plugin_ledger::{Append, Class, LedgerHandle, SeqRange, StepType, TrajId, WakeId};
use bough_plugin_llm::{
    AgentRequest, CallConfig, Chunk, LlmContentBlock, LlmHandle, LlmMessage, LlmRequest, LlmRole,
    RequestCall, RequestFacts, WakeKind,
};
use bough_plugin_rollups::{PassId, RollupsError};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use crate::{RollupRequest, SummarizerConfig, ROLLUP_REQUEST};

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
    /// The clock this call's own `rollup/request` step carries. Injected by the pass (AGENTS.md).
    pub at: chrono::DateTime<chrono::Utc>,
}

/// One model call's answer, with what it cost.
pub struct CallOutcome {
    pub text: String,
    pub tokens_in: u64,
    pub tokens_out: u64,
    pub source: TokenSource,
    /// The model the waterfall chose. Recorded, never chosen here.
    pub model: String,
}

/// sha256 of the rendered input, hex. What makes a replay able to prove the same input produced
/// the same block.
pub fn input_digest(system: &str, user: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(system.as_bytes());
    h.update([0u8]);
    h.update(user.as_bytes());
    format!("{:x}", h.finalize())
}

/// The synthetic wake id a governance pass runs under (P4-D2). Not a wake: §5's wake is an agent
/// TURN, and a pass is not one — but §3 puts a wake id on every step, so the pass carries its own.
pub fn pass_wake(pass: &PassId) -> WakeId {
    WakeId::new(pass.as_str())
}

/// Build the read-only facts for a governance call.
///
/// `model_override` is left `None` here and filled by the caller from the agent's ledger row: this
/// function is pure and the row is I/O.
pub fn facts(
    agent: &AgentName,
    traj: &TrajId,
    pass: &PassId,
    cfg: &SummarizerConfig,
    composition: &str,
) -> RequestFacts {
    RequestFacts {
        agent: agent.clone(),
        traj: traj.clone(),
        wake: pass_wake(pass),
        // §12: unattended work. This is the whole of P4-D3 — one boolean and one enum, and the
        // policy row does the rest.
        wake_kind: WakeKind::Scheduled,
        step_index: 0,
        answers_andrey: false,
        model_override: None,
        prompt_ver: cfg.prompt_ver.clone(),
        composition: composition.to_string(),
    }
}

/// One model call: run `agent/request` for the call config, `llm.stream` for the answer, append
/// the `rollup/request` step, return the assembled text and the token counts.
pub async fn call(
    ctx: &Context,
    llm: &LlmHandle,
    ledger: &LedgerHandle,
    req: CallRequest,
) -> Result<CallOutcome, RollupsError> {
    let decided = ctx
        .waterfall::<AgentRequest>(RequestCall {
            facts: req.facts.clone(),
            call: CallConfig {
                // EMPTY on purpose: the policy chooses. A governance row that named a model here
                // would be a second implementation of §12.
                model: String::new(),
                max_tokens: req.max_tokens,
                effort: None,
                // A recap is a jot, never work: tools are forbidden for the whole pass.
                tool_choice_none: true,
                meta: Default::default(),
            },
        })
        .await;
    let call_cfg = decided.call;
    if call_cfg.model.is_empty() {
        return Err(RollupsError::Model(
            "no `agent/request` listener chose a model for the governance pass; `model-policy` is \
             what does that (§12)"
                .to_string(),
        ));
    }
    let model = call_cfg.model.clone();

    let request = Arc::new(LlmRequest {
        projection_digest: None,
        model: model.clone(),
        system: Some(req.system.clone()),
        system_volatile: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text {
                text: req.user.clone(),
            }],
        }],
        tools: Vec::new(),
        call: call_cfg,
    });

    let mut stream = llm
        .stream(ctx, request.clone(), CancellationToken::new())
        .await;
    let mut text = String::new();
    let mut usage: Option<bough_plugin_llm::Usage> = None;
    let mut failure: Option<String> = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Chunk::TextDelta { text: t } => text.push_str(&t),
            Chunk::Usage(u) => usage = Some(u),
            Chunk::Failed(f) => {
                failure = Some(f.message.clone());
                break;
            }
            Chunk::End { .. } => break,
            // Reasoning and tool calls are not part of a recap's answer; a model that emits one
            // anyway is ignored rather than refused.
            _ => {}
        }
    }
    drop(stream);

    let (tokens_in, tokens_out, source) = match &usage {
        Some(u) => (
            u.input_tokens.max(0) as u64,
            u.output_tokens.max(0) as u64,
            TokenSource::Provider,
        ),
        // P4-D10: an estimate SAYS it is one.
        None => (
            (bough_plugin_projection::tokens::count(&req.system)
                + bough_plugin_projection::tokens::count(&req.user)) as u64,
            bough_plugin_projection::tokens::count(&text) as u64,
            TokenSource::Estimate,
        ),
    };

    // Model-visible ⟺ ledgered (§0.2): the request is reconstructible from (range, prompt_ver,
    // model), and this is the row that records the last two.
    ledger
        .0
        .append(Append {
            traj: req.facts.traj.clone(),
            wake: req.facts.wake.clone(),
            kind: StepType::new(ROLLUP_REQUEST),
            class: Class::Thought,
            body: serde_json::to_value(RollupRequest {
                pass: req.facts.wake.to_string(),
                phase: req.phase,
                prompt_ver: req.facts.prompt_ver.clone(),
                model: model.clone(),
                tier: req.tier,
                from_seq: req.range.from.0,
                to_seq: req.range.to.0,
                input_digest: input_digest(&req.system, &req.user),
                tokens_in,
                tokens_out,
                token_source: source,
                failed: failure.is_some(),
            })
            .expect("RollupRequest serialises"),
            cites: Vec::new(),
            at: req.at,
            id: None,
        })
        .await?;

    // The record is written FIRST, then the failure is reported: a failed call that left no row
    // is a billed call the ledger cannot see.
    if let Some(message) = failure {
        return Err(RollupsError::Model(message));
    }

    Ok(CallOutcome {
        text,
        tokens_in,
        tokens_out,
        source,
        model,
    })
}

/// A cite naming one rollup, in the ledger's `rollup:` scheme.
pub fn rollup_cite(id: &bough_plugin_ledger::RollupId) -> Cite {
    Cite {
        r#ref: Ref::new(format!("rollup:{id}")),
        url: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_is_stable_and_separates_the_two_halves() {
        assert_eq!(input_digest("a", "b"), input_digest("a", "b"));
        // Without the separator "ab"+"" and "a"+"b" would collide.
        assert_ne!(input_digest("ab", ""), input_digest("a", "b"));
    }

    #[test]
    fn governance_facts_are_unattended_and_name_no_model() {
        let f = facts(
            &AgentName::new("sol"),
            &TrajId::new("t"),
            &PassId::new("pass:1"),
            &crate::bundle_config(),
            "comp",
        );
        assert!(!f.answers_andrey, "a pass never answers Andrey");
        assert_eq!(f.wake_kind, WakeKind::Scheduled);
        assert_eq!(
            f.wake.as_str(),
            "pass:1",
            "the pass is its own wake (P4-D2)"
        );
        assert!(f.model_override.is_none());
    }
}
