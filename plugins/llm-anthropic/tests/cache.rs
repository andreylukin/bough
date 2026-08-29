//! Invariant under test (§12, the cache contract the tier split exists for): two rounds sharing a
//! byte-identical STABLE system tier, with only the VOLATILE tier changed between them, hit the
//! provider's prompt cache on the second round — `cache_write_tokens > 0` on the first,
//! `cache_read_tokens > 0` on the second. This is the LIVE half of the token-caching
//! verification; the offline half is `agent-loop`'s
//! `the_tail_band_rides_the_volatile_tier_and_never_moves_the_stable_system` (the split) and the
//! V4 invariant's `tiers_digest` check (the anchoring).
//!
//! P2-D27: `#[ignore]`d and gated on `BOUGH_LIVE=1`, so `make gates` stays offline. Run through
//! `make live`, or:
//!
//! `set -a; . ~/.bough/env; set +a; BOUGH_LIVE=1 cargo test -p bough-plugin-llm-anthropic -- --ignored cache`

use std::collections::BTreeMap;
use std::sync::Arc;

use bough_plugin_llm::{
    CallConfig, Chunk, LlmAdapter, LlmContentBlock, LlmMessage, LlmRequest, LlmRole,
};
use bough_plugin_llm_anthropic::{AnthropicAdapter, AnthropicConfig};
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

const HAIKU: &str = "claude-haiku-4-5-20251001";

/// A deterministic stable tier, comfortably past the provider's minimum cacheable prefix
/// (~4096 tokens for haiku): the same shape a real projection has, identity then tier summaries.
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
        model: HAIKU.into(),
        system: Some(stable_system()),
        system_volatile: Some(volatile.to_string()),
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text { text: text.into() }],
        }],
        tools: vec![],
        call: CallConfig {
            model: HAIKU.into(),
            max_tokens: 64,
            effort: None,
            tool_choice_none: false,
            meta: BTreeMap::new(),
        },
        projection_digest: None,
    })
}

async fn usage_of(adapter: &AnthropicAdapter, req: Arc<LlmRequest>) -> bough_llm::types::Usage {
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
#[ignore = "live: needs BOUGH_LIVE=1 and ANTHROPIC_API_KEY"]
async fn a_stable_tier_is_cache_read_on_the_second_round_across_a_volatile_change() {
    if std::env::var("BOUGH_LIVE").ok().as_deref() != Some("1") {
        eprintln!("BOUGH_LIVE is not 1; skipping");
        return;
    }
    let adapter = AnthropicAdapter::new(Arc::new(AnthropicConfig {
        models: "claude-*".into(),
        api_key_env: "ANTHROPIC_API_KEY".into(),
        base_url: None,
        request_timeout_ms: 60_000,
    }));

    // Round 1: a cold prefix. The stable tier (and the volatile tier's breakpoint) is written.
    let first = usage_of(
        &adapter,
        request("## Recent steps\n\nandrey: say ok\n", "say ok"),
    )
    .await;
    // Round 2: the SAME stable tier, a different volatile tail — the shape of the next wake.
    let second = usage_of(
        &adapter,
        request(
            "## Recent steps\n\nandrey: say ok\nsol: ok\nandrey: say ok again\n",
            "say ok again",
        ),
    )
    .await;

    let wrote = first.cache_write_tokens.unwrap_or(0);
    let read = second.cache_read_tokens.unwrap_or(0);
    assert!(
        wrote > 0,
        "the first round writes the cache; usage said cache_write={wrote} ({first:?})"
    );
    assert!(
        read > 0,
        "the second round re-reads the stable tier; usage said cache_read={read} ({second:?})"
    );
    // The read must cover the stable tier, not some accidental sliver: the stable system alone
    // is ~15k chars, so a read under 1000 tokens would mean the split is not what got cached.
    assert!(
        read >= 1000,
        "cache_read={read} is too small to be the stable tier ({second:?})"
    );
    eprintln!(
        "live cache verification: round1 write={wrote}, round2 read={read} (in={}/out={})",
        second.input_tokens, second.output_tokens
    );
}
