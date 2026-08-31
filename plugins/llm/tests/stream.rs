//! Invariant under test (§12, V10): a model failure LEAVES THIS SEAM AS A CHUNK, never as an
//! `Err`; a stream carries exactly one terminal chunk; and no shape of `llm/stream` listener can
//! make `stream()` hang. Plus §0.2's `resolve(request) -> Spec`: most specific wins, a tie is an
//! error naming both adapters.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_llm::{
    AdapterName, AdapterSpec, CallConfig, Chunk, FailureKind, LlmAdapter, LlmFailure, LlmHandle,
    LlmRequest, LlmSeamError, LlmStream, LlmStreamEvent, ModelMatch, StopReason, StreamCall,
};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

fn ctx() -> Context {
    Context::root(KernelCore::new())
}

fn req(model: &str) -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        projection_digest: None,
        model: model.to_string(),
        system: None,
        system_volatile: None,
        messages: vec![],
        tools: vec![],
        call: CallConfig {
            model: model.to_string(),
            max_tokens: 64,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
    })
}

/// An adapter that yields a fixed chunk list — including, on purpose, badly-shaped ones.
struct Canned {
    name: &'static str,
    chunks: Vec<Chunk>,
}

#[async_trait::async_trait]
impl LlmAdapter for Canned {
    fn name(&self) -> AdapterName {
        AdapterName::new(self.name)
    }
    async fn start(&self, _r: Arc<LlmRequest>, _c: CancellationToken) -> LlmStream {
        Box::pin(futures::stream::iter(self.chunks.clone()))
    }
}

fn spec(name: &'static str, matches: ModelMatch, chunks: Vec<Chunk>) -> AdapterSpec {
    AdapterSpec {
        name: AdapterName::new(name),
        matches,
        adapter: Arc::new(Canned { name, chunks }),
    }
}

fn ok_round() -> Vec<Chunk> {
    vec![
        Chunk::TextDelta {
            text: "hello".into(),
        },
        Chunk::End {
            stop: StopReason::EndTurn,
        },
    ]
}

async fn collect(h: &LlmHandle, ctx: &Context, model: &str) -> Vec<Chunk> {
    h.stream(ctx, req(model), CancellationToken::new())
        .await
        .collect()
        .await
}

// ---- the failure shape --------------------------------------------------------------------

#[tokio::test]
async fn a_failure_is_a_terminal_chunk_never_an_error() {
    let ctx = ctx();
    let h = LlmHandle::new();
    // Nothing is registered at all: the hardest case for "one failure shape", because the seam
    // itself is what went wrong.
    let got = collect(&h, &ctx, "claude-haiku-4-5-20251001").await;
    assert_eq!(got.len(), 1, "a refusal is one chunk: {got:?}");
    match &got[0] {
        Chunk::Failed(f) => {
            assert!(f.message.contains("no adapter"), "{}", f.message);
            assert!(!f.retryable);
        }
        other => panic!("a missing adapter must be a Failed chunk, got {other:?}"),
    }
    assert!(got[0].is_terminal());

    // And an adapter's own failure arrives the same way, so no caller branches twice.
    let h2 = LlmHandle::new();
    h2.adapter(
        &ctx,
        spec(
            "boom",
            ModelMatch::Any,
            vec![Chunk::Failed(LlmFailure {
                kind: FailureKind::Overloaded,
                message: "529".into(),
                retryable: true,
                status: Some(529),
                adapter: AdapterName::new("boom"),
            })],
        ),
    )
    .await
    .expect("registers");
    let got = collect(&h2, &ctx, "anything").await;
    assert!(matches!(got.as_slice(), [Chunk::Failed(f)] if f.retryable));
}

#[tokio::test]
async fn every_stream_ends_with_exactly_one_terminal_chunk() {
    let ctx = ctx();
    let h = LlmHandle::new();
    h.adapter(&ctx, spec("good", ModelMatch::Any, ok_round()))
        .await
        .expect("registers");
    let got = collect(&h, &ctx, "m").await;
    assert_eq!(got.iter().filter(|c| c.is_terminal()).count(), 1);
    assert!(
        got.last().expect("chunks").is_terminal(),
        "the terminal chunk is LAST: {got:?}"
    );

    // The invariant is the enforcement: a misbehaving adapter is REPORTED, not tolerated.
    bough_plugin_llm::invariant::clear();
    let bad = LlmHandle::new();
    bad.adapter(
        &ctx,
        spec(
            "two-terminals",
            ModelMatch::Any,
            vec![
                Chunk::End {
                    stop: StopReason::EndTurn,
                },
                Chunk::End {
                    stop: StopReason::ToolUse,
                },
            ],
        ),
    )
    .await
    .expect("registers");
    let _ = collect(&bad, &ctx, "m").await;
    let detail = bough_plugin_llm::invariant::evaluate(&bough_plugin_llm::invariant::seen())
        .expect_err("two terminal chunks must be reported");
    assert!(detail.contains("terminal"), "{detail}");
    bough_plugin_llm::invariant::clear();
}

// ---- the waterfall ------------------------------------------------------------------------

#[tokio::test]
async fn a_short_circuiting_wrapper_yields_a_failed_chunk() {
    let ctx = ctx();
    let h = LlmHandle::new();
    h.adapter(&ctx, spec("good", ModelMatch::Any, ok_round()))
        .await
        .expect("registers");
    // A listener that returns WITHOUT calling next() and WITHOUT filling the slot. The seam must
    // turn the empty value into a failure rather than waiting for a stream nobody will produce.
    ctx.on_waterfall::<LlmStreamEvent, _, _>(|c: StreamCall, _next| async move { c })
        .await
        .expect("listener registers");

    let got = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        collect(&h, &ctx, "claude-haiku-4-5-20251001"),
    )
    .await
    .expect("stream() must not hang on a short-circuiting wrapper");
    assert_eq!(got.len(), 1, "{got:?}");
    assert!(got[0].is_terminal());
}

#[tokio::test]
async fn a_wrapper_that_fills_the_slot_replaces_the_stream() {
    let ctx = ctx();
    let h = LlmHandle::new();
    h.adapter(&ctx, spec("good", ModelMatch::Any, ok_round()))
        .await
        .expect("registers");
    ctx.on_waterfall::<LlmStreamEvent, _, _>(|c: StreamCall, _next| async move {
        c.stream.put(Box::pin(futures::stream::iter(vec![
            Chunk::TextDelta {
                text: "replaced".into(),
            },
            Chunk::End {
                stop: StopReason::EndTurn,
            },
        ])));
        c
    })
    .await
    .expect("listener registers");
    let got = collect(&h, &ctx, "m").await;
    assert_eq!(
        got[0],
        Chunk::TextDelta {
            text: "replaced".into()
        }
    );
}

// ---- resolve(model) -> adapter --------------------------------------------------------------

#[tokio::test]
async fn adapter_resolution_picks_exact_over_prefix_over_any() {
    let ctx = ctx();
    let h = LlmHandle::new();
    h.adapter(&ctx, spec("any", ModelMatch::Any, ok_round()))
        .await
        .unwrap();
    assert_eq!(
        h.resolve("m").expect("any claims it").name().as_str(),
        "any"
    );

    h.adapter(
        &ctx,
        spec("prefix", ModelMatch::Prefix("claude-".into()), ok_round()),
    )
    .await
    .unwrap();
    assert_eq!(
        h.resolve("claude-haiku-4-5-20251001")
            .unwrap()
            .name()
            .as_str(),
        "prefix",
        "Prefix beats Any"
    );
    assert_eq!(h.resolve("gpt-5").unwrap().name().as_str(), "any");

    h.adapter(
        &ctx,
        spec(
            "exact",
            ModelMatch::Exact("claude-haiku-4-5-20251001".into()),
            ok_round(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(
        h.resolve("claude-haiku-4-5-20251001")
            .unwrap()
            .name()
            .as_str(),
        "exact",
        "Exact beats Prefix"
    );
    assert_eq!(h.adapters().len(), 3);
}

#[tokio::test]
async fn a_tie_is_reported_and_never_silently_last_wins() {
    let ctx = ctx();
    let h = LlmHandle::new();
    h.adapter(
        &ctx,
        spec("a", ModelMatch::Prefix("claude-".into()), ok_round()),
    )
    .await
    .unwrap();
    h.adapter(
        &ctx,
        spec("b", ModelMatch::Prefix("claude-".into()), ok_round()),
    )
    .await
    .unwrap();
    match h.resolve("claude-haiku-4-5-20251001") {
        Err(LlmSeamError::AmbiguousAdapter { a, b, .. }) => {
            let mut names = [a.to_string(), b.to_string()];
            names.sort();
            assert_eq!(names, ["a".to_string(), "b".to_string()]);
        }
        Ok(a) => panic!("a tie must be reported, got adapter `{}`", a.name()),
        Err(e) => panic!("a tie must be AmbiguousAdapter, got {e}"),
    }

    // And through `stream()` it is still ONE failure shape.
    let got = collect(&h, &ctx, "claude-haiku-4-5-20251001").await;
    assert!(matches!(got.as_slice(), [Chunk::Failed(_)]), "{got:?}");
}

#[tokio::test]
async fn a_missing_adapter_names_what_is_registered() {
    let ctx = ctx();
    let h = LlmHandle::new();
    h.adapter(
        &ctx,
        spec(
            "anthropic",
            ModelMatch::Prefix("claude-".into()),
            ok_round(),
        ),
    )
    .await
    .unwrap();
    match h.resolve("gpt-5") {
        Err(LlmSeamError::NoAdapter { registered, .. }) => {
            assert_eq!(registered, vec!["anthropic".to_string()])
        }
        Ok(a) => panic!("expected NoAdapter, got adapter `{}`", a.name()),
        Err(e) => panic!("expected NoAdapter, got {e}"),
    }
}

#[tokio::test]
async fn unloading_a_provider_removes_its_adapter() {
    let ctx = ctx();
    let h = LlmHandle::new();
    let handle = h
        .adapter(&ctx, spec("good", ModelMatch::Any, ok_round()))
        .await
        .expect("registers");
    assert_eq!(h.adapters().len(), 1);
    handle.dispose().await;
    assert!(h.adapters().is_empty(), "registration is an effect (§0.2)");
    assert!(matches!(
        h.resolve("m"),
        Err(LlmSeamError::NoAdapter { .. })
    ));
}
