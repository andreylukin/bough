//! V4 — the summarizer's cost per LIVED DAY, measured offline.
//!
//! `ledger-memory` + `llm-replay` for the run, this crate's own `rollup/request` steps for the
//! token counts, and `bough_llm::pricing::usage_cost_usd` over the vendored catalog for the
//! dollars. `#[ignore]`d and run by `make bench`.
//!
//! `cargo test -p bough-plugin-rollups-summarizer --test cost_bench -- --ignored --nocapture`

use crate::support;

use std::collections::BTreeMap;

use bough_llm::pricing::{usage_cost_usd, BillableTokens};
use bough_plugin_ledger::{Class, RollupKind, StepQuery, StepType};
use bough_plugin_rollups::Stop;
use bough_plugin_rollups_summarizer::{bundle_config, RollupRequest};
use chrono::Duration;
use support::*;

/// Above this the DESIGN is wrong, not the bench (docs/phase-4-plan.md §3, V4).
const CEILING_USD: f64 = 0.50;

/// A lived day: 6 wakes over 8 laptop-hours, ~35 steps each. The inter-wake gaps are above
/// `gap_minutes` and the intra-wake gaps below it, so the episode cut lands on wake boundaries
/// the way a real day does.
async fn synthetic_day(fx: &Fx) -> usize {
    let mut n = 0usize;
    for w in 0..6usize {
        // 8 hours / 6 wakes ≈ 80 minutes apart, comfortably above the 45-minute cut.
        let start = base() + Duration::minutes((w as i64) * 80);
        for i in 0..35usize {
            // Two minutes apart inside a wake: 68 minutes of work, every gap below the cut.
            let at = start + Duration::minutes((i as i64) * 2);
            match i % 5 {
                0 => {
                    fx.append(
                        w,
                        at,
                        "thought/text",
                        Class::Thought,
                        serde_json::json!({
                            "text": format!(
                                "wake {w} step {i}: reading the failing test and deciding what to \
                                 try next, which is the shape of most of a real day"),
                            "step_index": i as u32
                        }),
                        vec![],
                    )
                    .await;
                }
                4 => {
                    fx.append(
                        w,
                        at,
                        "action/done",
                        Class::Evidence,
                        serde_json::json!({
                            "action": bough_plugin_ledger::ActionId::new(format!("a{w}-{i}")),
                            "status": "done",
                            "artifact": null
                        }),
                        vec![bough_plugin_ledger::Cite {
                            r#ref: bough_plugin_ledger::Ref::new(format!("gh:o/r#{w}")),
                            url: None,
                        }],
                    )
                    .await;
                }
                _ => {
                    fx.append(
                        w,
                        at,
                        "thought/text",
                        Class::Thought,
                        serde_json::json!({
                            "text": format!("wake {w} step {i}: ran the suite, read the output"),
                            "step_index": i as u32
                        }),
                        vec![],
                    )
                    .await;
                }
            }
            n += 1;
        }
    }
    n
}

/// A transcript with NO usage chunks, so the recorded token counts are the harness's own estimate
/// over the REAL rendered prompt and the real answer. That is what makes this a measurement of
/// the summarizer rather than of the fixture; `provider_reported_usage_reaches_the_step` below
/// covers the other half of P4-D10.
fn recaps(n: usize) -> serde_json::Value {
    serde_json::Value::Array(
        (0..n)
            .map(|i| {
                serde_json::json!({
                    "chunks": [
                        { "type": "text", "text": format!(
                            "Recap {i}. The wake worked through a failing suite: the cause was \
                             narrowed to one call, a fix was tried, and the test went green. What \
                             is still open is whether the fix holds under the other provider.\n\
                             ## Open question\nWhether the second provider agrees.") },
                        { "type": "end", "stop": "end_turn" }
                    ]
                })
            })
            .collect(),
    )
}

#[tokio::test]
#[ignore = "bench: run by `make bench`"]
async fn cost_per_lived_day_bench() {
    // The BUNDLE's values, not a test's: a bench that measured a config nobody ships would
    // measure nothing.
    let cfg = bundle_config();
    let fx = fx_with(cfg.clone(), recaps(400)).await;
    let steps = synthetic_day(&fx).await;

    // To `Stop::NothingToDo`: `max_calls_per_pass` means a lived day takes several passes, which
    // is what the schedule hook will do too (P4-D14, P4-D16).
    let mut passes = 0usize;
    let mut calls = 0usize;
    loop {
        let report = fx.seal().await;
        passes += 1;
        calls += report.calls;
        if report.stop == Stop::NothingToDo || passes > 64 {
            break;
        }
    }

    let requests = fx
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("rollup/request")],
            ..Default::default()
        })
        .await
        .expect("a read");
    assert_eq!(requests.len(), calls, "every model call is ledgered (§0.2)");

    let mut by_model: BTreeMap<String, (u64, u64)> = BTreeMap::new();
    let mut sources: BTreeMap<String, usize> = BTreeMap::new();
    for s in &requests {
        let body: RollupRequest =
            serde_json::from_value((*s.body).clone()).expect("this crate's own body parses");
        let e = by_model.entry(body.model.clone()).or_default();
        e.0 += body.tokens_in;
        e.1 += body.tokens_out;
        *sources
            .entry(format!("{:?}", body.token_source).to_lowercase())
            .or_default() += 1;
    }

    let (mut tokens_in, mut tokens_out, mut usd) = (0u64, 0u64, 0.0f64);
    for (model, (tin, tout)) in &by_model {
        tokens_in += tin;
        tokens_out += tout;
        usd += usage_cost_usd(
            model,
            &BillableTokens {
                input_tokens: *tin as i64,
                output_tokens: *tout as i64,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        )
        .unwrap_or_else(|| panic!("`{model}` is not in the vendored pricing catalog"));
    }

    let rollups = fx.rollups().await;
    let tier1 = rollups
        .iter()
        .filter(|r| r.kind == RollupKind::Tier && r.tier == 1)
        .count();
    let tier2 = rollups
        .iter()
        .filter(|r| r.kind == RollupKind::Tier && r.tier == 2)
        .count();

    println!(
        "cost_per_lived_day steps={steps} windows={tier1} calls={calls} passes={passes} \
         tier1={tier1} tier2={tier2} tokens_in={tokens_in} tokens_out={tokens_out} \
         usd={usd:.4} token_source={sources:?} models={:?}",
        by_model.keys().collect::<Vec<_>>()
    );

    // The regression guard: a summarizer that starts calling the model per step fails HERE.
    assert!(
        calls <= steps / cfg.min_window_steps,
        "{calls} calls for {steps} steps is more than one per {} steps",
        cfg.min_window_steps
    );
    assert!(
        usd < CEILING_USD,
        "a lived day costs ${usd:.4}, over the ${CEILING_USD:.2} ceiling: the design is wrong, \
         not the bench"
    );
    assert!(tier1 > 0, "a lived day sealed no tier-1 block at all");
}

/// The other half of P4-D10, and the reason `llm-replay` grew a `Usage` variant: when the provider
/// reports usage, the `rollup/request` step records the provider's numbers and SAYS they are the
/// provider's.
#[tokio::test]
async fn provider_reported_usage_reaches_the_step() {
    let fx = fx(cfg(), 8).await;
    fx.seed(2, 10).await;
    fx.seal().await;
    let requests = fx
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new("rollup/request")],
            ..Default::default()
        })
        .await
        .expect("a read");
    assert_eq!(requests.len(), 1);
    let body: RollupRequest = serde_json::from_value((*requests[0].body).clone()).expect("a body");
    assert_eq!(body.tokens_in, 1_200, "the transcript's own numbers");
    assert_eq!(body.tokens_out, 180);
    assert_eq!(
        body.token_source,
        bough_plugin_rollups_summarizer::call::TokenSource::Provider
    );
    assert_eq!(body.model, MODEL, "the policy chose it, not the summarizer");
    assert_eq!(body.prompt_ver, cfg().prompt_ver);
    assert!(!body.input_digest.is_empty());

    // And with no usage chunk the count is an ESTIMATE, and says so (§16).
    let fx2 = fx_with(
        cfg(),
        serde_json::json!([{ "chunks": [
            { "type": "text", "text": "A recap of the episode." },
            { "type": "end", "stop": "end_turn" }
        ] }]),
    )
    .await;
    fx2.seed(2, 10).await;
    fx2.seal().await;
    let body: RollupRequest = serde_json::from_value(
        (*fx2
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![traj()],
                kinds: vec![StepType::new("rollup/request")],
                ..Default::default()
            })
            .await
            .expect("a read")[0]
            .body)
            .clone(),
    )
    .expect("a body");
    assert_eq!(
        body.token_source,
        bough_plugin_rollups_summarizer::call::TokenSource::Estimate
    );
    assert!(body.tokens_in > 0 && body.tokens_out > 0);
}
