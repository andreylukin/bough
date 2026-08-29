//! Invariant under test (§12): the OpenAI adapter is `llm-anthropic`'s twin through the seam's
//! SHARED mapping — a round leaves as the same ordered chunk vocabulary, a failure is a terminal
//! `Failed` chunk attributed to THIS adapter, and an absent key is `Failed { Auth }` (P2-D7),
//! never a panic and never a boot failure. Everything here runs against `bough-llm`'s fake
//! client or a cleared key; the offline suite never touches the network.

use std::sync::Arc;

use bough_llm::error::LlmError;
use bough_llm::test_support::fake_client;
use bough_llm::types::{LlmBlock, LlmResult, Usage};
use bough_plugin_llm::{
    CallConfig, Chunk, FailureKind, LlmAdapter, LlmContentBlock, LlmMessage, LlmRequest, LlmRole,
    StopReason,
};
use bough_plugin_llm_openai::{OpenaiAdapter, OpenaiConfig};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

const MODEL: &str = "openai:gpt-5.6-luna";

fn cfg() -> Arc<OpenaiConfig> {
    Arc::new(OpenaiConfig {
        models: "openai:*".into(),
        api_key_env: "OPENAI_API_KEY".into(),
        base_url: None,
        request_timeout_ms: 30_000,
    })
}

fn req(text: &str) -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        projection_digest: None,
        model: MODEL.into(),
        system: Some("You are terse.".into()),
        system_volatile: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text { text: text.into() }],
        }],
        tools: vec![],
        call: CallConfig {
            model: MODEL.into(),
            max_tokens: 256,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
    })
}

async fn drive(script: Vec<Result<LlmResult, LlmError>>, r: Arc<LlmRequest>) -> Vec<Chunk> {
    let (client, _calls) = fake_client(script);
    let a = OpenaiAdapter::with_client(cfg(), client);
    a.start(r, CancellationToken::new()).await.collect().await
}

#[tokio::test]
async fn a_round_maps_through_the_shared_seam_vocabulary() {
    let round = LlmResult {
        content: vec![
            LlmBlock::Text {
                text: "streamed already".into(),
            },
            LlmBlock::ToolUse {
                id: "call_1".into(),
                name: "run".into(),
                input: serde_json::json!({ "program": "1+1" }),
            },
        ],
        stop_reason: "tool_use".into(),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
            reasoning_tokens: None,
            cost_usd: None,
        }),
    };
    let chunks = drive(vec![Ok(round)], req("go")).await;
    assert!(
        matches!(
            chunks.last(),
            Some(Chunk::End {
                stop: StopReason::ToolUse
            })
        ),
        "{chunks:?}"
    );
    assert!(
        chunks
            .iter()
            .any(|c| matches!(c, Chunk::ToolCall { name, .. } if name.as_str() == "run")),
        "{chunks:?}"
    );
    assert!(
        chunks.iter().any(|c| matches!(c, Chunk::Usage(_))),
        "{chunks:?}"
    );
}

#[tokio::test]
async fn a_failure_is_one_terminal_chunk_attributed_to_this_adapter() {
    let chunks = drive(
        vec![Err(LlmError::with("rate limited", 429, None))],
        req("go"),
    )
    .await;
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    match &chunks[0] {
        Chunk::Failed(f) => {
            assert_eq!(f.kind, FailureKind::RateLimit);
            assert!(f.retryable, "{f:?}");
            assert_eq!(f.adapter.as_str(), "llm-openai");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// P2-D7: no key, no request, a terminal `Auth` failure — through the REAL client path (no
/// injected client), which is what proves the row never throws over a credential.
#[tokio::test]
async fn an_absent_key_is_a_terminal_auth_failure_not_a_panic() {
    let a = OpenaiAdapter::new(Arc::new(OpenaiConfig {
        models: "openai:*".into(),
        // An env var no machine sets: the absent-key path without touching the developer's env.
        api_key_env: "BOUGH_TEST_NO_SUCH_OPENAI_KEY".into(),
        base_url: None,
        request_timeout_ms: 5_000,
    }));
    let chunks: Vec<Chunk> = a
        .start(req("go"), CancellationToken::new())
        .await
        .collect()
        .await;
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    match &chunks[0] {
        Chunk::Failed(f) => {
            assert_eq!(f.kind, FailureKind::Auth, "{f:?}");
            assert!(!f.retryable, "a missing key will still be missing");
        }
        other => panic!("expected Failed {{ Auth }}, got {other:?}"),
    }
}
