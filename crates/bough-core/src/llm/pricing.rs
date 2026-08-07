//! The vendored cost and context-window catalog (port of `src/llm/pricing.ts`).
//!
//! The invariant this module holds is that **a price is a lookup, never a
//! negotiation**: `pricing.json` is a snapshot committed to the repo, so a
//! cost figure never depends on the network being up. A model the snapshot
//! does not know is reported as `None` — an honest "we don't price this" —
//! rather than silently costed at zero, because a zero would read as "free"
//! in the status bar and that is a lie the user cannot detect.
//!
//! Second invariant, and the reason `catalog_key` is public: **the catalog is
//! keyed by the same routing the client uses.** `routing.rs` decides which
//! provider a model id belongs to; this file has to reach the same conclusion
//! to find the row — the drift test in `routing.rs` pins the two together.
//!
//! The catalog is auto-derived from the models.dev snapshot: keys are
//! `"provider/model-id"`, values are
//! `[input, output, cacheRead, cacheWrite, contextWindow]` — dollars per
//! million tokens, then a token count. `null` in a rate slot means the
//! catalog has no separate rate for it, which we resolve to the input rate.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use tokio_util::sync::CancellationToken;

use crate::errors::BoughError;
use crate::schema::parts::Usage;
use crate::types::{LlmClient, LlmParams, LlmResult, OnText};

/// The vendored catalog, verbatim from `src/llm/pricing.json`.
pub const PRICING_JSON: &str = include_str!("pricing.json");

/// `[input, output, cacheRead, cacheWrite, contextWindow]`.
type Row = (f64, f64, Option<f64>, Option<f64>, Option<i64>);

static ROWS: LazyLock<HashMap<String, Row>> =
    LazyLock::new(|| serde_json::from_str(PRICING_JSON).expect("vendored pricing.json parses"));

/// USD per million tokens. Cache rates fall back to the input rate when the
/// catalog has none.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostRates {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

/// The token counts a round can be billed for. Mirrors `Usage`, nullish
/// included (nullish = 0, not NaN).
#[derive(Clone, Copy, Debug, Default)]
pub struct BillableTokens {
    pub input_tokens: i64,
    pub output_tokens: i64,
    /// Included in `input_tokens`; re-priced at the discounted read rate.
    pub cache_read_tokens: Option<i64>,
    /// Included in `input_tokens`; re-priced at the write rate.
    pub cache_write_tokens: Option<i64>,
}

impl From<&Usage> for BillableTokens {
    fn from(u: &Usage) -> Self {
        BillableTokens {
            input_tokens: u.input_tokens,
            output_tokens: u.output_tokens,
            cache_read_tokens: u.cache_read_tokens,
            cache_write_tokens: u.cache_write_tokens,
        }
    }
}

/// The catalog keys a bough model id could match, most specific first.
///
/// Mirrors `provider_for` in `routing.rs`: an `openai:x` id is OpenAI proper,
/// a `@cf/x` id is Cloudflare Workers AI, any other `vendor/model` id is
/// routed through OpenRouter, and a bare id is Anthropic. The OpenRouter case
/// tries the `openrouter/` key first and then the bare `vendor/model` key,
/// because models.dev also lists many of those vendors directly and the
/// direct row is a usable fallback when OpenRouter has not published its own.
pub fn catalog_keys(model: &str) -> Vec<String> {
    if let Some(bare) = model.strip_prefix("openai:") {
        return vec![format!("openai/{bare}")];
    }
    // `@cf/…` before the slash test, exactly as `provider_for` orders them.
    if model.starts_with("@cf/") {
        return vec![format!("cloudflare-workers-ai/{model}")];
    }
    if model.contains('/') {
        return vec![format!("openrouter/{model}"), model.to_string()];
    }
    vec![format!("anthropic/{model}")]
}

/// The single key a model id resolved to, or `None` when nothing matched.
pub fn catalog_key(model: &str) -> Option<String> {
    catalog_keys(model)
        .into_iter()
        .find(|k| ROWS.contains_key(k))
}

fn row_for(model: &str) -> Option<&'static Row> {
    catalog_key(model).and_then(|key| ROWS.get(&key))
}

/// Whether the vendored snapshot knows this model at all.
pub fn is_priced(model: &str) -> bool {
    catalog_key(model).is_some()
}

/// Rates for a bough model id; `None` when the catalog does not price it.
pub fn rates_for(model: &str) -> Option<CostRates> {
    let (input, output, cache_read, cache_write, _) = *row_for(model)?;
    Some(CostRates {
        input,
        output,
        cache_read: cache_read.unwrap_or(input),
        cache_write: cache_write.unwrap_or(input),
    })
}

/// The model's context window in tokens; `None` when the catalog does not
/// know it. The turn runner uses this to name the limit in a context-overflow
/// error — which is why an unknown window must stay `None` rather than
/// defaulting to some plausible number that would produce a confidently wrong
/// error message.
pub fn context_window_for(model: &str) -> Option<i64> {
    row_for(model).and_then(|r| r.4)
}

/// Dollar cost of one round. `input_tokens` arrives INCLUSIVE of cache reads
/// and writes — every provider client normalizes it that way so the context
/// meter can show the true prompt size — so the cached share is subtracted
/// out and re-priced at its own rate. Returns `None` when the model is not in
/// the catalog. The fresh share is clamped at zero: over-reported cache reads
/// must not produce a negative bill.
pub fn usage_cost_usd(model: &str, u: &BillableTokens) -> Option<f64> {
    let r = rates_for(model)?;
    let read = u.cache_read_tokens.unwrap_or(0) as f64;
    let write = u.cache_write_tokens.unwrap_or(0) as f64;
    let fresh = (u.input_tokens as f64 - read - write).max(0.0);
    Some(
        (fresh * r.input
            + read * r.cache_read
            + write * r.cache_write
            + u.output_tokens as f64 * r.output)
            / 1e6,
    )
}

// ---- the pricing decorator --------------------------------------------------

struct Pricing {
    inner: Arc<dyn LlmClient>,
}

#[async_trait::async_trait]
impl LlmClient for Pricing {
    async fn run(
        &self,
        params: LlmParams,
        on_text: OnText,
        cancel: CancellationToken,
    ) -> Result<LlmResult, BoughError> {
        let model = params.model.clone();
        let mut result = self.inner.run(params, on_text, cancel).await?;
        if let Some(usage) = &mut result.usage {
            if usage.cost_usd.is_none() {
                // Unpriced model → an explicit None, never 0 — but the field
                // stays None either way; the wire omits it only when absent,
                // and TS stamps an explicit null. Usage's Option<f64> carries
                // both readings; what matters is that no 0.0 is invented.
                usage.cost_usd = usage_cost_usd(&model, &BillableTokens::from(&*usage));
            }
        }
        Ok(result)
    }
}

/// Stamp `costUsd` on a round's usage from the vendored catalog.
///
/// One wrapper rather than three call sites: the three providers report
/// tokens in three shapes but they all normalize to `Usage` before this runs,
/// so pricing has exactly one implementation and an unpriced model degrades
/// identically on every route (`cost_usd: None`, never a silent zero).
pub fn with_pricing(inner: Arc<dyn LlmClient>) -> Arc<dyn LlmClient> {
    Arc::new(Pricing { inner })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::test_support::{fake_client, params_for_model, TOOLS};
    use crate::types::LlmBlock;

    #[test]
    fn catalog_keys_mirrors_the_clients_routing_rule() {
        assert_eq!(
            catalog_keys("claude-opus-5"),
            vec!["anthropic/claude-opus-5"]
        );
        assert_eq!(catalog_keys("openai:gpt-5"), vec!["openai/gpt-5"]);
        // OpenRouter first, then the vendor's own models.dev row as a
        // fallback — many vendors are listed directly and that row is usable
        // when OpenRouter has none.
        assert_eq!(
            catalog_keys("google/gemini-2.5-pro"),
            vec!["openrouter/google/gemini-2.5-pro", "google/gemini-2.5-pro"]
        );
        assert_eq!(
            catalog_keys("@cf/zai-org/glm-5.2"),
            vec!["cloudflare-workers-ai/@cf/zai-org/glm-5.2"]
        );
    }

    #[test]
    fn the_vendored_snapshot_prices_the_models_bough_ships_with() {
        for model in ["claude-opus-5", "claude-haiku-4-5", "openai:gpt-5"] {
            assert!(is_priced(model), "{model} should be in the catalog");
            let rates = rates_for(model).unwrap_or_else(|| panic!("{model}"));
            assert!(rates.input > 0.0 && rates.output > 0.0, "{model}");
        }
    }

    #[test]
    fn an_unknown_model_is_none_everywhere_never_zero() {
        let model = "no-such-vendor/no-such-model";
        assert!(!is_priced(model));
        assert_eq!(catalog_key(model), None);
        assert_eq!(rates_for(model), None);
        assert_eq!(context_window_for(model), None);
        assert_eq!(
            usage_cost_usd(
                model,
                &BillableTokens {
                    input_tokens: 1_000_000,
                    output_tokens: 1_000_000,
                    ..Default::default()
                }
            ),
            None
        );
    }

    #[test]
    fn cache_rates_fall_back_to_the_input_rate_when_the_catalog_has_none() {
        // openai/gpt-5 carries a null cacheWrite slot in the snapshot.
        let rates = rates_for("openai:gpt-5").unwrap();
        assert!(rates.cache_read > 0.0 && rates.cache_write > 0.0);
        assert!(
            rates.cache_read <= rates.input,
            "a cache read is never dearer than fresh input"
        );
        assert_eq!(
            rates.cache_write, rates.input,
            "null slot falls back to the input rate"
        );
    }

    #[test]
    fn context_window_for_reports_a_real_window_for_a_known_model() {
        let window = context_window_for("claude-opus-5").unwrap();
        assert!(window > 100_000, "got {window}");
    }

    #[test]
    fn usage_cost_usd_the_cached_share_is_subtracted_out_and_re_priced() {
        let model = "claude-opus-5";
        let r = rates_for(model).unwrap();
        // input_tokens arrives INCLUSIVE of reads and writes, so the fresh
        // share here is 1M - 400k - 100k = 500k.
        let cost = usage_cost_usd(
            model,
            &BillableTokens {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                cache_read_tokens: Some(400_000),
                cache_write_tokens: Some(100_000),
            },
        )
        .unwrap();
        let expected = (500_000.0 * r.input
            + 400_000.0 * r.cache_read
            + 100_000.0 * r.cache_write
            + 1_000_000.0 * r.output)
            / 1e6;
        assert!((cost - expected).abs() < 1e-9, "{cost} vs {expected}");
        // The whole point of the discount: the same tokens billed fresh cost more.
        let uncached = usage_cost_usd(
            model,
            &BillableTokens {
                input_tokens: 1_000_000,
                output_tokens: 1_000_000,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(uncached > cost);
    }

    #[test]
    fn usage_cost_usd_nullish_cache_counts_behave_like_zero_not_like_nan() {
        let with_nulls = usage_cost_usd(
            "claude-opus-5",
            &BillableTokens {
                input_tokens: 1000,
                output_tokens: 1000,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        );
        let without = usage_cost_usd(
            "claude-opus-5",
            &BillableTokens {
                input_tokens: 1000,
                output_tokens: 1000,
                ..Default::default()
            },
        );
        assert_eq!(with_nulls, without);
        assert!(with_nulls.unwrap().is_finite());
    }

    #[test]
    fn usage_cost_usd_an_over_counted_cache_share_cannot_drive_the_fresh_share_negative() {
        // Defensive: a provider that reports reads exceeding the total must
        // not produce a negative bill.
        let cost = usage_cost_usd(
            "claude-opus-5",
            &BillableTokens {
                input_tokens: 100,
                output_tokens: 0,
                cache_read_tokens: Some(900),
                cache_write_tokens: None,
            },
        )
        .unwrap();
        assert!(cost >= 0.0);
    }

    #[tokio::test]
    async fn with_pricing_stamps_cost_usd_from_the_vendored_catalog() {
        let (client, _calls) = fake_client(vec![Ok(LlmResult {
            content: vec![LlmBlock::Text { text: "hi".into() }],
            stop_reason: "end_turn".into(),
            usage: Some(Usage {
                input_tokens: 1_000_000,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_read_tokens: Some(0),
                cache_write_tokens: Some(0),
                cost_usd: None,
            }),
        })]);
        let result = with_pricing(client)
            .run(
                params_for_model("claude-opus-5", &TOOLS),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        let cost = result.usage.unwrap().cost_usd;
        assert!(matches!(cost, Some(c) if c > 0.0), "{cost:?}");
    }

    #[tokio::test]
    async fn with_pricing_leaves_an_unpriced_model_unpriced_rather_than_zero() {
        let (client, _calls) = fake_client(vec![Ok(LlmResult {
            content: vec![],
            stop_reason: "end_turn".into(),
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 100,
                reasoning_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost_usd: None,
            }),
        })]);
        let result = with_pricing(client)
            .run(
                params_for_model("no-such-vendor/no-such-model", &TOOLS),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.usage.unwrap().cost_usd, None, "never 0");
    }

    #[tokio::test]
    async fn with_pricing_never_overwrites_a_cost_the_provider_already_stamped() {
        let (client, _calls) = fake_client(vec![Ok(LlmResult {
            content: vec![],
            stop_reason: "end_turn".into(),
            usage: Some(Usage {
                input_tokens: 1_000_000,
                output_tokens: 0,
                reasoning_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                cost_usd: Some(0.5),
            }),
        })]);
        let result = with_pricing(client)
            .run(
                params_for_model("claude-opus-5", &TOOLS),
                Arc::new(|_| {}),
                CancellationToken::new(),
            )
            .await
            .unwrap();
        assert_eq!(result.usage.unwrap().cost_usd, Some(0.5));
    }
}
