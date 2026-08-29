//! Invariant under test (V10): the mapper turns one `bough-llm` round into the seam's chunk
//! vocabulary IN ORDER, and never throws — an absent API key is a terminal `Failed { Auth }`
//! chunk (P2-D7), not a panic and not a boot failure.
//!
//! Everything here runs against `bough-llm`'s own `test_support` fake client, so the offline suite
//! never touches the network. The one live case is `#[ignore]`d and gated on `BOUGH_LIVE=1`.

use std::sync::Arc;

use bough_llm::error::LlmError;
use bough_llm::test_support::fake_client;
use bough_llm::types::{LlmBlock, LlmResult, Usage};
use bough_plugin_llm::{
    CallConfig, Chunk, FailureKind, LlmAdapter, LlmContentBlock, LlmMessage, LlmRequest, LlmRole,
    LlmToolDef, StopReason,
};
use bough_plugin_llm_anthropic::{AnthropicAdapter, AnthropicConfig};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

const HAIKU: &str = "claude-haiku-4-5-20251001";

fn cfg() -> Arc<AnthropicConfig> {
    Arc::new(AnthropicConfig {
        models: "claude-*".into(),
        api_key_env: "ANTHROPIC_API_KEY".into(),
        base_url: None,
        request_timeout_ms: 30_000,
    })
}

fn req(text: &str, tools: Vec<LlmToolDef>) -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        projection_digest: None,
        model: HAIKU.into(),
        system: Some("You are terse.".into()),
        system_volatile: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text { text: text.into() }],
        }],
        tools,
        call: CallConfig {
            model: HAIKU.into(),
            max_tokens: 256,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
    })
}

async fn drive(script: Vec<Result<LlmResult, LlmError>>, r: Arc<LlmRequest>) -> Vec<Chunk> {
    let (client, _calls) = fake_client(script);
    let a = AnthropicAdapter::with_client(cfg(), client);
    a.start(r, CancellationToken::new()).await.collect().await
}

#[tokio::test]
async fn text_reasoning_tool_calls_and_usage_map_to_the_seam() {
    let round = LlmResult {
        content: vec![
            LlmBlock::Text {
                text: "looking".into(),
            },
            LlmBlock::Reasoning {
                text: "the file is small".into(),
                meta: Some(serde_json::json!({ "sig": "abc" })),
            },
            LlmBlock::ToolUse {
                id: "call_1".into(),
                name: "read_file".into(),
                input: serde_json::json!({ "path": "AGENTS.md" }),
            },
        ],
        stop_reason: "tool_use".into(),
        usage: Some(Usage {
            input_tokens: 120,
            output_tokens: 44,
            ..Default::default()
        }),
    };
    let got = drive(vec![Ok(round)], req("read AGENTS.md", vec![])).await;

    // The ORDER is the contract: streamed text first, then the round's trailing blocks.
    assert_eq!(
        got[0],
        Chunk::TextDelta {
            text: "looking".into()
        },
        "text streams live through on_text: {got:?}"
    );
    match &got[1] {
        Chunk::ReasoningDelta { text, meta } => {
            assert_eq!(text, "the file is small");
            assert_eq!(meta, &Some(serde_json::json!({ "sig": "abc" })));
        }
        other => panic!("expected reasoning, got {other:?}"),
    }
    match &got[2] {
        Chunk::ToolCall { id, name, input } => {
            assert_eq!(id.as_str(), "call_1");
            assert_eq!(name.as_str(), "read_file");
            assert_eq!(input, &serde_json::json!({ "path": "AGENTS.md" }));
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
    match &got[3] {
        Chunk::Usage(u) => assert_eq!((u.input_tokens, u.output_tokens), (120, 44)),
        other => panic!("expected usage, got {other:?}"),
    }
    assert_eq!(
        got[4],
        Chunk::End {
            stop: StopReason::ToolUse
        }
    );
    assert_eq!(got.len(), 5);
    assert_eq!(got.iter().filter(|c| c.is_terminal()).count(), 1);
}

#[tokio::test]
async fn an_absent_key_is_a_terminal_auth_failure() {
    // 401 is exactly what `bough_llm::routing::require_key` produces for an unset variable, so
    // this is the real path with the network removed rather than a stand-in for it.
    let got = drive(
        vec![Err(LlmError::with(
            "Anthropic: ANTHROPIC_API_KEY is not set — put it in ~/.bough/env",
            401,
            None,
        ))],
        req("hello", vec![]),
    )
    .await;
    match got.as_slice() {
        [Chunk::Failed(f)] => {
            assert_eq!(f.kind, FailureKind::Auth);
            assert!(!f.retryable);
            assert_eq!(f.status, Some(401));
            assert_eq!(f.adapter.as_str(), "llm-anthropic");
        }
        other => panic!("an absent key must be a Failed chunk, not a panic: {other:?}"),
    }
}

#[tokio::test]
async fn a_transport_failure_is_terminal_and_retryable() {
    let got = drive(
        vec![Err(LlmError::new("connection reset"))],
        req("hello", vec![]),
    )
    .await;
    match got.as_slice() {
        [Chunk::Failed(f)] => {
            assert_eq!(f.kind, FailureKind::Transport);
            assert!(f.retryable, "llm-retry reads this verdict");
        }
        other => panic!("{other:?}"),
    }
}

/// P2-D5: `bough-llm`'s own retry ladder is DISABLED, so one failure is one attempt. If both
/// layers retried, `agent/request-error`'s attempt counter would be a lie.
#[tokio::test]
async fn the_adapter_does_not_retry_on_its_own() {
    let (client, calls) = fake_client(vec![
        Err(LlmError::with("500 upstream", 500, None)),
        Ok(LlmResult {
            content: vec![LlmBlock::Text {
                text: "second attempt".into(),
            }],
            stop_reason: "end_turn".into(),
            usage: None,
        }),
    ]);
    let a = AnthropicAdapter::with_client(cfg(), client);
    let got: Vec<Chunk> = a
        .start(req("hello", vec![]), CancellationToken::new())
        .await
        .collect()
        .await;
    assert!(matches!(got.as_slice(), [Chunk::Failed(_)]), "{got:?}");
    assert_eq!(
        calls.lock().unwrap().len(),
        1,
        "exactly one round reached the client"
    );
}

/// The call config, not `req.model`, is what goes on the wire — `agent/request` listeners write
/// the call config, and the model policy (§12) is one of them.
#[tokio::test]
async fn the_call_config_is_what_reaches_the_client() {
    let (client, calls) = fake_client(vec![Ok(LlmResult {
        content: vec![],
        stop_reason: "end_turn".into(),
        usage: None,
    })]);
    let a = AnthropicAdapter::with_client(cfg(), client);
    let mut r = (*req("hello", vec![])).clone();
    r.call.model = "claude-opus-5".into();
    r.call.max_tokens = 77;
    r.call.tool_choice_none = true;
    let _: Vec<Chunk> = a
        .start(Arc::new(r), CancellationToken::new())
        .await
        .collect()
        .await;
    let sent = calls.lock().unwrap();
    assert_eq!(sent[0].model, "claude-opus-5");
    assert_eq!(sent[0].max_tokens, 77);
    assert!(sent[0].tool_choice_none);
    assert_eq!(sent[0].system.as_deref(), Some("You are terse."));
}

// ---- live ------------------------------------------------------------------------------------

/// P2-D27: `#[ignore]`d and gated on `BOUGH_LIVE=1`, so `make gates` stays offline.
///
/// `set -a; . ~/.bough/env; set +a; BOUGH_LIVE=1 cargo test -p bough-plugin-llm-anthropic -- --ignored`
#[tokio::test]
#[ignore = "live: needs BOUGH_LIVE=1 and ANTHROPIC_API_KEY"]
async fn a_live_haiku_round_streams_text_tool_calls_and_usage() {
    if std::env::var("BOUGH_LIVE").ok().as_deref() != Some("1") {
        eprintln!("BOUGH_LIVE is not 1; skipping");
        return;
    }
    let tools = vec![LlmToolDef {
        name: "record_answer".into(),
        description: "Record the final answer.".into(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": { "answer": { "type": "string" } },
            "required": ["answer"],
            "additionalProperties": false,
        }),
    }];
    let a = AnthropicAdapter::new(cfg());
    let got: Vec<Chunk> = a
        .start(
            req(
                "Say the word ready, then call record_answer with answer=\"ready\".",
                tools,
            ),
            CancellationToken::new(),
        )
        .await
        .collect()
        .await;

    if let Some(Chunk::Failed(f)) = got.first() {
        panic!("live round failed: {} ({:?})", f.message, f.kind);
    }
    assert_eq!(
        got.iter().filter(|c| c.is_terminal()).count(),
        1,
        "exactly one terminal chunk: {got:?}"
    );
    assert!(
        got.last().expect("chunks").is_terminal(),
        "the terminal chunk is LAST"
    );
    assert!(
        got.iter().any(|c| matches!(c, Chunk::TextDelta { .. })),
        "haiku streamed no text: {got:?}"
    );
    assert!(
        got.iter()
            .any(|c| matches!(c, Chunk::ToolCall { name, .. } if name.as_str() == "record_answer")),
        "haiku made no tool call: {got:?}"
    );
    match got.iter().find(|c| matches!(c, Chunk::Usage(_))) {
        Some(Chunk::Usage(u)) => assert!(u.input_tokens > 0 && u.output_tokens > 0, "{u:?}"),
        _ => panic!("no usage chunk: {got:?}"),
    }
}
