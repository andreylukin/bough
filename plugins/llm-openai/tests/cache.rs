//! Invariant under test (§12): the OpenAI Responses API's AUTOMATIC prefix caching sees the tier
//! split too — two rounds sharing a byte-identical stable system with only the volatile tier
//! changed report `cache_read_tokens > 0` on the second round. The anthropic twin
//! (`llm-anthropic/tests/cache.rs`) proves the breakpoint path; this one proves the
//! no-breakpoints path, since OpenAI caches any repeated prefix past ~1024 tokens.
//!
//! P2-D27: `#[ignore]`d and gated on `BOUGH_LIVE=1`. Run through `make live`, or:
//!
//! `set -a; . ~/.bough/env; set +a; BOUGH_LIVE=1 cargo test -p bough-plugin-llm-openai -- --ignored cache`

use std::sync::Arc;

use bough_plugin_llm::{
    CallConfig, Chunk, LlmAdapter, LlmContentBlock, LlmMessage, LlmRequest, LlmRole,
};
use bough_plugin_llm_openai::{OpenaiAdapter, OpenaiConfig};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

/// The cheap model of Andrey's own OpenAI setup (`~/.bough/model.json`'s `cheapModel`).
const MODEL: &str = "openai:gpt-5.6-luna";

fn stable_system() -> String {
    let mut s = String::from(
        "## Identity\n\nYou are sol, a resident agent. Answer in one short sentence.\n\n\
         ## Tier summaries\n\n",
    );
    for i in 0..400 {
        s.push_str(&format!(
            "- [{i}] the {i}th prior stretch of work concerned rebuilding the harness: the \
             ledger stayed append-only, the projection was assembled from sections, and every \
             model-visible byte was written down before the model saw it.\n"
        ));
    }
    s
}

fn request(volatile: &str, text: &str) -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        model: MODEL.into(),
        system: Some(stable_system()),
        system_volatile: Some(volatile.to_string()),
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text { text: text.into() }],
        }],
        tools: vec![],
        call: CallConfig {
            model: MODEL.into(),
            max_tokens: 64,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
        projection_digest: None,
    })
}

async fn usage_of(adapter: &OpenaiAdapter, req: Arc<LlmRequest>) -> bough_llm::types::Usage {
    let mut stream = adapter.start(req, CancellationToken::new()).await;
    let mut usage = None;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Chunk::Usage(u) => usage = Some(u),
            Chunk::Failed(f) => panic!("the live round failed: {f:?}"),
            _ => {}
        }
    }
    usage.expect("a live round reports usage")
}

#[tokio::test]
#[ignore = "live: needs BOUGH_LIVE=1 and OPENAI_API_KEY"]
async fn a_stable_tier_is_cache_read_on_the_second_round_across_a_volatile_change() {
    if std::env::var("BOUGH_LIVE").ok().as_deref() != Some("1") {
        eprintln!("BOUGH_LIVE is not 1; skipping");
        return;
    }
    let adapter = OpenaiAdapter::new(Arc::new(OpenaiConfig {
        models: "openai:*".into(),
        api_key_env: "OPENAI_API_KEY".into(),
        base_url: None,
        request_timeout_ms: 60_000,
    }));

    let first = usage_of(
        &adapter,
        request("## Recent steps\n\nandrey: say ok\n", "say ok"),
    )
    .await;
    // The Responses cache is written asynchronously: a second round fired immediately after a
    // COLD first can still read 0 (observed live, 2026-08-29). Three tries with a pause is the
    // honest shape of "the cache works", not a flake-hider: a split that does not cache reads 0
    // on every try.
    let mut second = None;
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(std::time::Duration::from_secs(3)).await;
        }
        let u = usage_of(
            &adapter,
            request(
                "## Recent steps\n\nandrey: say ok\nsol: ok\nandrey: say ok again\n",
                "say ok again",
            ),
        )
        .await;
        let done = u.cache_read_tokens.unwrap_or(0) >= 1000;
        second = Some(u);
        if done {
            break;
        }
    }
    let second = second.expect("at least one second round ran");

    let read = second.cache_read_tokens.unwrap_or(0);
    assert!(
        read >= 1000,
        "the second round re-reads the stable tier automatically; usage said \
         cache_read={read} (first={first:?}, second={second:?})"
    );
    eprintln!(
        "live openai cache verification: round2 read={read} (in={}/out={})",
        second.input_tokens, second.output_tokens
    );
}
