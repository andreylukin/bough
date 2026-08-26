//! §5's one calibration obligation on this phase: "recalibrate [the 0.6 headroom factor] against
//! own trajectories in Phase 1" (P1-D20).
//!
//! The estimate `projection` budgets with is o200k_base (`tiktoken-rs`), which is not Anthropic's
//! tokenizer. What the headroom factor has to buy is that a request which FITS THE ESTIMATE still
//! fits the real window. So: assemble a projection from this build's OWN recorded trajectory,
//! count it with `tokens::count`, ask Anthropic's `count_tokens` endpoint for the true count
//! through `bough-llm`'s transport, print `o200k=N anthropic=M ratio=R`, and assert
//! `R <= 1 / headroom`. If R ever exceeds it, the fix is one number in `bundles/bough-base.yml`
//! and a regenerated golden set — not a change here.
//!
//! `BOUGH_LIVE=1` only: an offline gate cannot call the API, so `make gates` never depends on it.

mod support;

use std::sync::Arc;

use bough_llm::routing::{require_key, Provider, ProviderOpts};
use bough_llm::sse::HttpRequest;
use bough_plugin_hello::trace;
use bough_plugin_projection::{tokens, AssembleRequest, Projection};

/// The shipped factor. Read from `bundles/bough-base.yml` rather than typed here, so the assertion
/// is about what actually ships.
const HEADROOM_KEY: &str = "headroom:";

/// The model both `sol` and `terra` are during this build.
const MODEL: &str = "claude-haiku-4-5-20251001";

/// A trajectory big enough that the two tokenizers have something to disagree about.
const P1: &str = "\
- id: ledger
  plugin: ledger-sqlite
  config:
    path: !!expr 'bough_path(\"ledger.db\")'
    busy_timeout_ms: 5000
- id: projection
  plugin: projection-assembler
  config:
    budget_tokens: 160000
    headroom: 0.6
    tail_steps: 60
    tail_floor_steps: 10
    mail_newest_n: 5
    max_tiers: 3
    file_view_dir: !!expr 'bough_path(\"views\")'
- id: probe
  plugin: projection-probe
  config:
    traj: t1
    agent: a1
    steps: 200
";

fn live() -> bool {
    std::env::var("BOUGH_LIVE").ok().as_deref() == Some("1")
}

/// The `headroom` the shipped base bundle sets.
fn shipped_headroom() -> f32 {
    let text = std::fs::read_to_string(support::repo_root().join("bundles/bough-base.yml"))
        .expect("the shipped base bundle is readable");
    let line = text
        .lines()
        .find(|l| l.trim().starts_with(HEADROOM_KEY))
        .expect("the shipped bundle sets a headroom factor");
    line.trim()
        .trim_start_matches(HEADROOM_KEY)
        .trim()
        .parse()
        .expect("the headroom factor is a number")
}

/// Assemble the probe agent's projection through the live binding and return its text.
async fn own_trajectory_text() -> String {
    let (kernel, _dir) = support::boot_with(P1).await;
    let handle = kernel
        .root()
        .peek_live::<Projection>()
        .expect("projection is bound")
        .as_ref()
        .clone();
    let text = handle
        .0
        .assemble(&AssembleRequest {
            agent: bough_plugin_ledger::AgentName::new("a1"),
            wake: None,
            at: chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .into(),
            budget: None,
        })
        .await
        .expect("the projection assembles")
        .to_text();
    kernel.shutdown().await;
    text
}

/// Anthropic's own count for `text`, through `bough-llm`'s injected transport and key resolution.
/// Nothing here prints or returns the key.
async fn anthropic_tokens(text: &str) -> usize {
    let opts = ProviderOpts::default();
    let env = opts.env_or_default();
    let transport = opts.transport_or_default();
    let key = require_key(&env, Provider::Anthropic, &["ANTHROPIC_AUTH_TOKEN"])
        .expect("BOUGH_LIVE=1 requires an Anthropic key in ~/.bough/env");
    let base = env("ANTHROPIC_API_BASE").unwrap_or_else(|| "https://api.anthropic.com".into());
    let body = serde_json::json!({
        "model": MODEL,
        "messages": [{ "role": "user", "content": text }],
    });
    let res = transport
        .fetch(HttpRequest {
            url: format!("{base}/v1/messages/count_tokens"),
            headers: vec![
                ("x-api-key".into(), key),
                ("anthropic-version".into(), "2023-06-01".into()),
                ("content-type".into(), "application/json".into()),
            ],
            body: Some(body.to_string()),
        })
        .await
        .expect("the count_tokens request reaches the API");
    let status = res.status;
    let ok = res.ok();
    // `text()` consumes the response, so the body is drained once and used for both paths.
    let body = res.text().await;
    assert!(ok, "count_tokens returned {status}: {body}");
    let payload: serde_json::Value =
        serde_json::from_str(&body).expect("count_tokens answers JSON");
    payload
        .get("input_tokens")
        .and_then(|v| v.as_u64())
        .expect("count_tokens answers `input_tokens`") as usize
}

/// The measurement itself, shared by the two cases so the API is called once per case and both
/// report the same number.
async fn measure() -> (usize, usize, f64) {
    let text = own_trajectory_text().await;
    assert!(
        text.len() > 500,
        "the calibration needs a real trajectory to measure, not a stub:\n{text}"
    );
    let o200k = tokens::count(&text);
    let anthropic = anthropic_tokens(&text).await;
    assert!(o200k > 0 && anthropic > 0);
    let ratio = anthropic as f64 / o200k as f64;
    println!("o200k={o200k} anthropic={anthropic} ratio={ratio:.3}");
    (o200k, anthropic, ratio)
}

/// `#[ignore]` and not an early `return`: a skipped test that reports `ok` is indistinguishable
/// from coverage. Run with `BOUGH_LIVE=1 cargo test -- --ignored`.
#[tokio::test]
#[ignore = "live: needs the API; run with BOUGH_LIVE=1 --ignored"]
async fn o200k_estimate_stays_within_the_headroom_factor() {
    assert!(live(), "set BOUGH_LIVE=1 to run the token calibration");
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();

    let headroom = shipped_headroom();
    let (_o200k, _anthropic, ratio) = measure().await;
    let bound = 1.0 / headroom as f64;
    assert!(
        ratio <= bound,
        "the o200k estimate under-counts by {ratio:.3}x, past the {bound:.3}x the shipped \
         headroom {headroom} buys; lower `headroom` in bundles/bough-base.yml and regenerate the \
         goldens"
    );
}

/// `#[ignore]` for the same reason as its sibling.
#[tokio::test]
#[ignore = "live: needs the API; run with BOUGH_LIVE=1 --ignored"]
async fn the_measured_ratio_is_printed_and_recorded() {
    assert!(live(), "set BOUGH_LIVE=1 to run the token calibration");
    let _guard = trace::test_lock();
    bough_plugin_projection_probe::clear();

    let (o200k, anthropic, ratio) = measure().await;
    // The printed line is the artefact P1-D20 asks for; the recorded one is what makes the 0.6 in
    // the bundle a MEASURED number rather than an inherited one.
    println!("MEASURED o200k={o200k} anthropic={anthropic} ratio={ratio:.3}");

    let build = std::fs::read_to_string(support::repo_root().join("BUILD.md"))
        .expect("BUILD.md is readable");
    assert!(
        build.contains("o200k→anthropic ratio"),
        "BUILD.md's Phase 1 row must record the measured o200k→anthropic ratio (P1-D20)"
    );
}

/// Keeps the `Arc` import honest when the two live tests are skipped.
#[allow(dead_code)]
fn _transport_is_shared() -> Option<Arc<dyn bough_llm::sse::Transport>> {
    None
}
