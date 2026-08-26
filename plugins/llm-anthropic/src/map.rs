//! Invariant: the mapping from `bough-llm`'s round surface to the seam's chunk vocabulary is
//! TOTAL and ordered — text deltas as they stream, then reasoning, tool calls and usage as the
//! round returns (P2-D6) — and every exit from it is exactly one terminal chunk.

use bough_plugin_llm::{Chunk, LlmFailure, LlmRequest};

/// Map a finished `bough-llm` round onto the trailing chunks (reasoning, tool calls, usage, and
/// the terminal `End`). Text deltas have already streamed through `on_text`.
///
/// Pure, so the mapping is testable without a client. WP-1.
pub fn round_to_chunks(_result: &bough_llm::types::LlmResult) -> Vec<Chunk> {
    todo!("WP-1: map blocks + usage + stop reason onto the chunk vocabulary")
}

/// Map a `bough-llm` error onto a terminal [`LlmFailure`], including the retryable verdict
/// `llm-retry` reads.
///
/// WP-1.
pub fn error_to_failure(_e: &bough_llm::error::LlmError) -> LlmFailure {
    todo!("WP-1: classify the error into FailureKind + retryable")
}

/// Map a seam request onto `bough-llm`'s params. WP-1.
pub fn request_to_params(_req: &LlmRequest) -> bough_llm::types::LlmParams {
    todo!("WP-1: system/system_volatile/messages/tools/call -> LlmParams")
}
