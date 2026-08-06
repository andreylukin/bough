//! OpenAI proper — the Responses API. v1 STUB (plan rows 1.12 / 3.15).
//!
//! Routing still resolves an `openai:` id here; running answers a trait-shaped
//! non-retryable 401 until wave 3 ports the Responses client (distinct wire
//! shape: `instructions`, `store: false`, reasoning items replayed verbatim
//! via `include: ["reasoning.encrypted_content"]` — see spec llm.md §3b).

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::errors::BoughError;
use crate::llm::routing::ProviderOpts;
use crate::types::{LlmClient, LlmParams, LlmResult, OnText};

struct OpenAIStub;

#[async_trait::async_trait]
impl LlmClient for OpenAIStub {
    async fn run(
        &self,
        _params: LlmParams,
        _on_text: OnText,
        _cancel: CancellationToken,
    ) -> Result<LlmResult, BoughError> {
        // 401 so `is_retryable` says no — a stubbed provider will still be
        // stubbed in 15 seconds.
        Err(BoughError::llm_with("openai: provider not configured", 401, None))
    }
}

pub fn openai_client(_opts: ProviderOpts) -> Arc<dyn LlmClient> {
    Arc::new(OpenAIStub)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::test_support::{params_over, TOOLS};

    #[tokio::test]
    async fn the_openai_stub_answers_provider_not_configured_401() {
        let client = openai_client(ProviderOpts::default());
        let err = client
            .run(
                params_over("openai:gpt-5", &TOOLS, |_| {}),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap_err();
        assert_eq!(err.status(), 401, "must not be retried");
        assert_eq!(err.to_string(), "openai: provider not configured");
        assert!(!crate::llm::retry::is_retryable(&err));
    }
}
