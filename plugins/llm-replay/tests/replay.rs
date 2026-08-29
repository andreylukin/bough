//! Invariant under test (V10): replay is DETERMINISTIC — the same transcript answers the same
//! requests the same way, in every process and on every run — and an unmatched request fails
//! LOUDLY in strict mode rather than yielding a silent empty answer.

use std::sync::Arc;

use bough_plugin_llm::{
    CallConfig, Chunk, FailureKind, LlmAdapter, LlmContentBlock, LlmMessage, LlmRequest, LlmRole,
    StopReason,
};
use bough_plugin_llm_replay::{ReplayAdapter, ReplayConfig, Transcript};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

const TRANSCRIPT: &str = r#"
rounds:
  - match: "plan"
    chunks:
      - { type: reasoning, text: "thinking about the plan" }
      - { type: text, text: "here is the plan" }
      - { type: tool_call, id: "c1", name: "bash", input: { cmd: "ls" } }
      - { type: end, stop: tool_use }
  - match: "done"
    chunks:
      - { type: text, text: "finished" }
      - { type: end, stop: end_turn }
"#;

fn cfg(strict: bool) -> Arc<ReplayConfig> {
    Arc::new(ReplayConfig {
        transcript: None,
        rounds: None,
        strict,
        models: "*".into(),
        delay_ms: 0,
    })
}

fn req(text: &str) -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        projection_digest: None,
        model: "claude-haiku-4-5-20251001".into(),
        system: None,
        system_volatile: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text { text: text.into() }],
        }],
        tools: vec![],
        call: CallConfig {
            model: "claude-haiku-4-5-20251001".into(),
            max_tokens: 128,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
    })
}

fn adapter(strict: bool) -> ReplayAdapter {
    ReplayAdapter::new(cfg(strict), Transcript::parse(TRANSCRIPT).expect("parses"))
}

async fn run(a: &ReplayAdapter, text: &str) -> Vec<Chunk> {
    a.start(req(text), CancellationToken::new())
        .await
        .collect()
        .await
}

#[tokio::test]
async fn the_same_transcript_answers_deterministically() {
    let mut runs = Vec::new();
    for _ in 0..2 {
        let a = adapter(true);
        runs.push(vec![
            run(&a, "make a plan").await,
            run(&a, "are we done").await,
        ]);
    }
    assert_eq!(runs[0], runs[1], "two runs of one transcript must agree");

    // And the answer is the recorded one, in the recorded order.
    let first = &runs[0][0];
    assert!(matches!(first[0], Chunk::ReasoningDelta { .. }));
    assert!(matches!(first[1], Chunk::TextDelta { .. }));
    match &first[2] {
        Chunk::ToolCall { id, name, input } => {
            assert_eq!(id.as_str(), "c1");
            assert_eq!(name.as_str(), "bash");
            assert_eq!(input, &serde_json::json!({ "cmd": "ls" }));
        }
        other => panic!("expected a tool call, got {other:?}"),
    }
    assert_eq!(
        first[3],
        Chunk::End {
            stop: StopReason::ToolUse
        }
    );
    assert_eq!(first.iter().filter(|c| c.is_terminal()).count(), 1);
}

#[tokio::test]
async fn an_unmatched_request_fails_in_strict_mode() {
    let a = adapter(true);
    let got = run(&a, "something nobody recorded").await;
    match got.as_slice() {
        [Chunk::Failed(f)] => {
            assert_eq!(f.kind, FailureKind::BadRequest);
            assert!(f.message.contains("no unconsumed round"), "{}", f.message);
            assert!(!f.retryable);
        }
        other => panic!("strict mode must refuse, got {other:?}"),
    }
}

#[tokio::test]
async fn a_lenient_replay_ends_the_turn_instead_of_hanging() {
    let a = adapter(false);
    let got = run(&a, "something nobody recorded").await;
    assert_eq!(
        got,
        vec![Chunk::End {
            stop: StopReason::EndTurn
        }]
    );
}

#[tokio::test]
async fn a_round_without_a_terminal_chunk_is_closed_by_the_adapter() {
    // The transcript is DATA: a fixture that forgot its `end` must not produce a stream the seam's
    // invariant would report as never terminating.
    let a = ReplayAdapter::new(
        cfg(true),
        Transcript::parse(r#"- chunks: [{ type: text, text: "unterminated" }]"#).expect("parses"),
    );
    let got = run(&a, "anything").await;
    assert!(got.last().expect("chunks").is_terminal(), "{got:?}");
    assert_eq!(got.iter().filter(|c| c.is_terminal()).count(), 1);
}

#[tokio::test]
async fn a_transcript_file_and_inline_rounds_load_the_same_rounds() {
    let dir = tempfile::tempdir().expect("a temp dir");
    let path = dir.path().join("t.yml");
    std::fs::write(&path, TRANSCRIPT).expect("write");
    let from_file = ReplayAdapter::load(&ReplayConfig {
        transcript: Some(path),
        rounds: None,
        strict: true,
        models: "*".into(),
        delay_ms: 0,
    })
    .expect("loads");
    let inline = ReplayAdapter::load(&ReplayConfig {
        transcript: None,
        rounds: Some(serde_json::to_value(&Transcript::parse(TRANSCRIPT).unwrap().rounds).unwrap()),
        strict: true,
        models: "*".into(),
        delay_ms: 0,
    })
    .expect("loads");
    assert_eq!(from_file, inline);
}

#[tokio::test]
async fn an_unreadable_transcript_is_refused_not_silently_empty() {
    let err = ReplayAdapter::load(&ReplayConfig {
        transcript: Some("/nonexistent/transcript.yml".into()),
        rounds: None,
        strict: true,
        models: "*".into(),
        delay_ms: 0,
    })
    .expect_err("a missing transcript must fail loud (§0.2)");
    assert!(err.contains("cannot read transcript"), "{err}");
}

/// P3-D20 / V1: a delayed round ARRIVES rather than lands. The first chunk is on the stream
/// while the rest of the answer is still to come — which is what makes "it streams" assertable
/// offline, with no live model.
#[tokio::test]
async fn a_delayed_round_yields_its_chunks_over_time() {
    const DELAYED: &str = r#"
rounds:
  - chunks:
      - { type: text, text: "first " }
      - { type: text, text: "second", delay_ms: 120 }
      - { type: end, stop: end_turn }
"#;
    let adapter = ReplayAdapter::new(
        cfg(true),
        Transcript::parse(DELAYED).expect("the transcript parses"),
    );
    let started = std::time::Instant::now();
    let mut stream = adapter
        .start(req("anything"), CancellationToken::new())
        .await;

    let first = stream.next().await.expect("a first chunk");
    assert!(
        matches!(&first, Chunk::TextDelta { text } if text == "first "),
        "the head of the answer is on the stream first"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_millis(100),
        "the first chunk did not wait for the rest of the answer"
    );

    let rest: Vec<Chunk> = stream.collect().await;
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(110),
        "the delayed chunk really waited: {:?}",
        started.elapsed()
    );
    assert!(
        matches!(&rest[0], Chunk::TextDelta { text } if text == "second"),
        "and then the tail arrived"
    );
    assert!(matches!(
        rest.last(),
        Some(Chunk::End {
            stop: StopReason::EndTurn
        })
    ));
}

/// The default is 0, so every Phase-2 transcript behaves exactly as it did.
#[tokio::test]
async fn an_undelayed_round_still_lands_at_once() {
    let adapter = ReplayAdapter::new(
        cfg(true),
        Transcript::parse(TRANSCRIPT).expect("the transcript parses"),
    );
    let started = std::time::Instant::now();
    let chunks: Vec<Chunk> = adapter
        .start(req("plan"), CancellationToken::new())
        .await
        .collect()
        .await;
    assert_eq!(chunks.len(), 4);
    assert!(started.elapsed() < std::time::Duration::from_millis(50));
}
