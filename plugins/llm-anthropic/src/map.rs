//! Invariant: the mapping from `bough-llm`'s round surface to the seam's chunk vocabulary is
//! TOTAL and ordered — text deltas as they stream, then reasoning, tool calls and usage as the
//! round returns (P2-D6) — and every exit from it is exactly one terminal chunk.
//!
//! The mapping itself lives on the Definition (`bough_plugin_llm::adapt`) so this row and
//! `llm-openai` cannot drift on it; what stays here is this adapter's NAME on a failure, and the
//! tests that pinned the behavior when this crate owned the code.

use bough_plugin_llm::{AdapterName, Chunk, LlmFailure, LlmRequest, StopReason};

/// [`bough_plugin_llm::adapt::round_to_chunks`], re-exported under this crate's old path.
pub fn round_to_chunks(result: &bough_llm::types::LlmResult) -> Vec<Chunk> {
    bough_plugin_llm::adapt::round_to_chunks(result)
}

/// [`bough_plugin_llm::adapt::stop_reason`], re-exported under this crate's old path.
pub fn stop_reason(s: &str) -> StopReason {
    bough_plugin_llm::adapt::stop_reason(s)
}

/// [`bough_plugin_llm::adapt::error_to_failure`], attributed to THIS adapter.
pub fn error_to_failure(e: &bough_llm::error::LlmError) -> LlmFailure {
    bough_plugin_llm::adapt::error_to_failure(&AdapterName::new(crate::PLUGIN_NAME), e)
}

/// [`bough_plugin_llm::adapt::request_to_params`], re-exported under this crate's old path.
pub fn request_to_params(req: &LlmRequest) -> bough_llm::types::LlmParams {
    bough_plugin_llm::adapt::request_to_params(req)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_llm::error::LlmError;
    use bough_llm::types::{LlmBlock, LlmResult, Usage};
    use bough_plugin_llm::FailureKind;

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
