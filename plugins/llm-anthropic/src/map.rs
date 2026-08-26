//! Invariant: the mapping from `bough-llm`'s round surface to the seam's chunk vocabulary is
//! TOTAL and ordered — text deltas as they stream, then reasoning, tool calls and usage as the
//! round returns (P2-D6) — and every exit from it is exactly one terminal chunk.

use bough_plugin_llm::{
    AdapterName, Chunk, FailureKind, LlmFailure, LlmRequest, StopReason, ToolCallId, ToolName,
};

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

/// Map a `bough-llm` error onto a terminal [`LlmFailure`], including the retryable verdict
/// `llm-retry` reads.
///
/// The verdict is `bough_llm::retry::is_retryable`'s, not a second table: two tables would let a
/// deployment retry what the provider layer already decided is hopeless.
pub fn error_to_failure(e: &bough_llm::error::LlmError) -> LlmFailure {
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
        adapter: AdapterName::new(crate::PLUGIN_NAME),
    }
}

/// Map a seam request onto `bough-llm`'s params.
///
/// `call.model` wins over `req.model`: `agent/request` listeners write the CALL CONFIG, and the
/// model policy (§12) is exactly such a listener, so the config is what actually goes on the wire.
pub fn request_to_params(req: &LlmRequest) -> bough_llm::types::LlmParams {
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

#[cfg(test)]
mod tests {
    use super::*;
    use bough_llm::error::LlmError;
    use bough_llm::types::{LlmBlock, LlmResult, Usage};

    #[test]
    fn a_round_maps_reasoning_then_tool_calls_then_usage_then_end() {
        let r = LlmResult {
            content: vec![
                LlmBlock::Text {
                    text: "streamed already".into(),
                },
                LlmBlock::Reasoning {
                    text: "because".into(),
                    meta: Some(serde_json::json!({ "sig": "x" })),
                },
                LlmBlock::ToolUse {
                    id: "c1".into(),
                    name: "bash".into(),
                    input: serde_json::json!({ "cmd": "ls" }),
                },
            ],
            stop_reason: "tool_use".into(),
            usage: Some(Usage {
                input_tokens: 10,
                output_tokens: 3,
                ..Default::default()
            }),
        };
        let got = round_to_chunks(&r);
        assert!(matches!(got[0], Chunk::ReasoningDelta { .. }));
        assert!(matches!(got[1], Chunk::ToolCall { .. }));
        assert!(matches!(got[2], Chunk::Usage(_)));
        assert_eq!(
            got[3],
            Chunk::End {
                stop: StopReason::ToolUse
            }
        );
        assert_eq!(
            got.iter().filter(|c| c.is_terminal()).count(),
            1,
            "exactly one terminal chunk"
        );
        assert!(
            !got.iter().any(|c| matches!(c, Chunk::TextDelta { .. })),
            "text already streamed through on_text; re-emitting it would double the thought steps"
        );
    }

    #[test]
    fn a_missing_key_is_auth_and_not_retryable() {
        // 401 is what `routing::require_key` produces.
        let f = error_to_failure(&LlmError::with("ANTHROPIC_API_KEY is not set", 401, None));
        assert_eq!(f.kind, FailureKind::Auth);
        assert!(!f.retryable, "a missing key will still be missing in 15s");
    }

    #[test]
    fn transient_statuses_are_retryable_and_named() {
        for (status, kind) in [
            (429, FailureKind::RateLimit),
            (529, FailureKind::Overloaded),
            (502, FailureKind::Transport),
        ] {
            let f = error_to_failure(&LlmError::with("x", status, None));
            assert_eq!(f.kind, kind, "status {status}");
            assert!(f.retryable, "status {status} is retryable");
        }
        let cancelled = error_to_failure(&LlmError::with("aborted", 499, None));
        assert_eq!(cancelled.kind, FailureKind::Cancelled);
        assert!(!cancelled.retryable);
    }

    #[test]
    fn an_unknown_stop_word_still_stops() {
        assert_eq!(stop_reason("who knows"), StopReason::EndTurn);
        assert_eq!(stop_reason("max_tokens"), StopReason::MaxTokens);
    }
}
