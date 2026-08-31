//! Invariant under test (§12): the OpenRouter adapter is `llm-openai`'s twin through the seam's
//! SHARED mapping — a round leaves as the same ordered chunk vocabulary, a failure is a terminal
//! `Failed` chunk attributed to THIS adapter, an absent key is `Failed { Auth }` (P2-D7), and the
//! `openrouter:` prefix is stripped to the wire's `vendor/model` while a slashless id is refused
//! as `BadRequest` rather than misrouted. Everything here runs against `bough-llm`'s fake client
//! or a cleared key; the offline suite never touches the network.

use std::sync::Arc;

use bough_llm::error::LlmError;
use bough_llm::test_support::fake_client;
use bough_llm::types::{LlmBlock, LlmResult, Usage};
use bough_plugin_llm::{
    CallConfig, Chunk, FailureKind, LlmAdapter, LlmContentBlock, LlmMessage, LlmRequest, LlmRole,
    StopReason,
};
use bough_plugin_llm_openrouter::{OpenrouterAdapter, OpenrouterConfig};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

const MODEL: &str = "openrouter:openai/gpt-5";

fn cfg() -> Arc<OpenrouterConfig> {
    Arc::new(OpenrouterConfig {
        models: "openrouter:*".into(),
        api_key_env: "OPENROUTER_API_KEY".into(),
        base_url: None,
        request_timeout_ms: 30_000,
    })
}

fn req_for(model: &str, text: &str) -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        projection_digest: None,
        model: model.into(),
        system: Some("You are terse.".into()),
        system_volatile: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text { text: text.into() }],
        }],
        tools: vec![],
        call: CallConfig {
            model: model.into(),
            max_tokens: 256,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
    })
}

async fn drive(script: Vec<Result<LlmResult, LlmError>>, r: Arc<LlmRequest>) -> Vec<Chunk> {
    let (client, _calls) = fake_client(script);
    let a = OpenrouterAdapter::with_client(cfg(), client);
    a.start(r, CancellationToken::new()).await.collect().await
}

#[tokio::test]
async fn a_round_maps_through_the_shared_seam_vocabulary() {
    let round = LlmResult {
        content: vec![LlmBlock::Text {
            text: "streamed already".into(),
        }],
        stop_reason: "end_turn".into(),
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 5,
            cache_read_tokens: Some(0),
            cache_write_tokens: Some(0),
            reasoning_tokens: None,
            cost_usd: None,
        }),
    };
    let chunks = drive(vec![Ok(round)], req_for(MODEL, "go")).await;
    assert!(
        matches!(
            chunks.last(),
            Some(Chunk::End {
                stop: StopReason::EndTurn
            })
        ),
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
        req_for(MODEL, "go"),
    )
    .await;
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    match &chunks[0] {
        Chunk::Failed(f) => {
            assert_eq!(f.kind, FailureKind::RateLimit);
            assert!(f.retryable, "{f:?}");
            assert_eq!(f.adapter.as_str(), "llm-openrouter");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// The prefix rule: `openrouter:gpt-5` has no vendor half, and sending it on would MISROUTE to
/// Anthropic inside `client_for` — so it leaves as one terminal `BadRequest` naming the shape.
#[tokio::test]
async fn a_slashless_id_is_one_terminal_bad_request_never_a_misroute() {
    let chunks = drive(vec![], req_for("openrouter:gpt-5", "go")).await;
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    match &chunks[0] {
        Chunk::Failed(f) => {
            assert_eq!(f.kind, FailureKind::BadRequest, "{f:?}");
            assert_eq!(f.adapter.as_str(), "llm-openrouter");
            assert!(f.message.contains("vendor/model"), "{f:?}");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}

/// P2-D7: no key, no request, a terminal `Auth` failure — through the REAL client path (no
/// injected client), which is what proves the row never throws over a credential.
#[tokio::test]
async fn an_absent_key_is_a_terminal_auth_failure_not_a_panic() {
    let a = OpenrouterAdapter::new(Arc::new(OpenrouterConfig {
        models: "openrouter:*".into(),
        // An env var no machine sets: the absent-key path without touching the developer's env.
        api_key_env: "BOUGH_TEST_NO_SUCH_OPENROUTER_KEY".into(),
        base_url: None,
        request_timeout_ms: 5_000,
    }));
    let chunks: Vec<Chunk> = a
        .start(req_for(MODEL, "go"), CancellationToken::new())
        .await
        .collect()
        .await;
    assert_eq!(chunks.len(), 1, "{chunks:?}");
    match &chunks[0] {
        Chunk::Failed(f) => {
            assert_eq!(f.kind, FailureKind::Auth, "{f:?}");
            assert_eq!(f.adapter.as_str(), "llm-openrouter");
        }
        other => panic!("expected Failed, got {other:?}"),
    }
}
