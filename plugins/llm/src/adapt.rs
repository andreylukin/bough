//! Invariant: the mapping between `bough-llm`'s round surface and this seam's chunk vocabulary is
//! TOTAL, ordered (text deltas as they stream, then reasoning, tool calls and usage as the round
//! returns, P2-D6), and SHARED — it lives on the Definition so two provider rows (`llm-anthropic`,
//! `llm-openai`) cannot drift on the part that must not drift (the `collect-core` precedent).
//! Every exit is exactly one terminal chunk.

use crate::ids::{ToolCallId, ToolName};
use crate::stream::{Chunk, FailureKind, LlmFailure, StopReason};
use crate::AdapterName;

/// Map a finished `bough-llm` round onto the trailing chunks (reasoning, tool calls, usage, and
/// the terminal `End`). Text deltas have already streamed through `on_text`.
///
/// Pure, so the mapping is testable without a client.
pub fn round_to_chunks(result: &bough_llm::types::LlmResult) -> Vec<Chunk> {
    use bough_llm::types::LlmBlock;
    let mut out = Vec::new();
    for b in &result.content {
        match b {
            // Already streamed through `on_text`; re-emitting it would double the thought steps.
            LlmBlock::Text { .. } => {}
            LlmBlock::Reasoning { text, meta } => out.push(Chunk::ReasoningDelta {
                text: text.clone(),
                meta: meta.clone(),
            }),
            LlmBlock::ToolUse { id, name, input } => out.push(Chunk::ToolCall {
                id: ToolCallId::new(id),
                name: ToolName::new(name),
                input: input.clone(),
            }),
        }
    }
    if let Some(u) = &result.usage {
        out.push(Chunk::Usage(u.clone()));
    }
    out.push(Chunk::End {
        stop: stop_reason(&result.stop_reason),
    });
    out
}

/// The provider's stop word. Unknown words are `end_turn`: a round that stopped for a reason this
/// binary has never heard of still STOPPED, and inventing a failure would be a worse lie.
pub fn stop_reason(s: &str) -> StopReason {
    match s {
        "tool_use" => StopReason::ToolUse,
        "max_tokens" => StopReason::MaxTokens,
        "stop_sequence" => StopReason::StopSequence,
        _ => StopReason::EndTurn,
    }
}

/// Map a `bough-llm` error onto a terminal [`LlmFailure`] attributed to `adapter`, including the
/// retryable verdict `llm-retry` reads.
///
/// The verdict is `bough_llm::retry::is_retryable`'s, not a second table: two tables would let a
/// deployment retry what the provider layer already decided is hopeless.
pub fn error_to_failure(adapter: &AdapterName, e: &bough_llm::error::LlmError) -> LlmFailure {
    let kind = match e.status() {
        401 | 403 => FailureKind::Auth,
        // `sse::aborted` — the caller's own cancellation.
        499 => FailureKind::Cancelled,
        429 => FailureKind::RateLimit,
        529 => FailureKind::Overloaded,
        502..=504 => FailureKind::Transport,
        400 if e.message.contains("context") || e.message.contains("too long") => {
            FailureKind::ContextOverflow
        }
        400 => FailureKind::BadRequest,
        408 => FailureKind::Transport,
        _ => FailureKind::Other,
    };
    LlmFailure {
        kind,
        message: e.message.clone(),
        retryable: bough_llm::retry::is_retryable(e),
        status: Some(e.status()),
        adapter: adapter.clone(),
    }
}

/// Map a seam request onto `bough-llm`'s params.
///
/// `call.model` wins over `req.model`: `agent/request` listeners write the CALL CONFIG, and the
/// model policy (§12) is exactly such a listener, so the config is what actually goes on the wire.
pub fn request_to_params(req: &crate::request::LlmRequest) -> bough_llm::types::LlmParams {
    bough_llm::types::LlmParams {
        model: req.call.model.clone(),
        system: req.system.clone(),
        system_volatile: req.system_volatile.clone(),
        max_tokens: req.call.max_tokens,
        messages: req.messages.clone(),
        tools: req.tools.clone(),
        tool_choice_none: req.call.tool_choice_none,
        effort: req.call.effort,
    }
}
