//! Invariant: an unknown price is reported as UNKNOWN, never as zero (phase ux1 §2.10, M24).
//! This row owns the price table because it already owns which model runs.

use bough_plugin_llm::Usage;

/// What one model costs, per million tokens.
///
/// The table itself is CONFIG (`model.policy.prices`), never a constant here: prices change and a
/// deployment must be able to correct one with a patch. The row this build ships is Anthropic's
/// published Claude Haiku 4.5 list price — $1.00 / MTok input, $5.00 / MTok output, $0.10 / MTok
/// cache read, $1.25 / MTok 5-minute cache write — and it belongs in `bundles/bough-tui-app.yml`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_write_per_mtok: f64,
}

/// PURE: usage × price. `None` when the model has no row in the table.
///
/// The provider's own number wins when it gives one: it knows about discounts and batch rates
/// this table cannot. Otherwise the four buckets are priced per million tokens. Cache buckets
/// that the provider did not report are ZERO tokens — not an unknown price, which is why a
/// missing bucket does not make the whole round unknown.
pub fn cost_usd(u: &Usage, p: Option<&Price>) -> Option<f64> {
    if let Some(c) = u.cost_usd {
        return Some(c);
    }
    let p = p?;
    let per = |tokens: i64, rate: f64| (tokens.max(0) as f64) * rate / 1_000_000.0;
    Some(
        per(u.input_tokens, p.input_per_mtok)
            + per(u.output_tokens, p.output_per_mtok)
            + per(u.cache_read_tokens.unwrap_or(0), p.cache_read_per_mtok)
            + per(u.cache_write_tokens.unwrap_or(0), p.cache_write_per_mtok),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> Usage {
        Usage {
            input_tokens: 1_000_000,
            output_tokens: 1_000_000,
            reasoning_tokens: None,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_usd: None,
        }
    }

    fn price() -> Price {
        Price {
            input_per_mtok: 1.0,
            output_per_mtok: 5.0,
            cache_read_per_mtok: 0.1,
            cache_write_per_mtok: 1.25,
        }
    }

    #[test]
    fn an_unpriced_model_costs_an_unknown_amount_never_zero() {
        assert_eq!(cost_usd(&usage(), None), None);
    }

    #[test]
    fn the_four_buckets_are_priced_per_million_tokens() {
        let mut u = usage();
        u.cache_read_tokens = Some(1_000_000);
        u.cache_write_tokens = Some(1_000_000);
        let c = cost_usd(&u, Some(&price())).unwrap();
        assert!((c - 7.35).abs() < 1e-9, "{c}");
    }

    #[test]
    fn the_providers_own_number_wins_when_it_gives_one() {
        let mut u = usage();
        u.cost_usd = Some(0.25);
        assert_eq!(cost_usd(&u, Some(&price())), Some(0.25));
        assert_eq!(cost_usd(&u, None), Some(0.25));
    }
}
