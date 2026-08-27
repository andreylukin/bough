//! Invariant: an unknown price is reported as UNKNOWN, never as zero (phase ux1 §2.10, M24).
//! This row owns the price table because it already owns which model runs.

use bough_plugin_llm::Usage;

/// What one model costs, per million tokens.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Price {
    pub input_per_mtok: f64,
    pub output_per_mtok: f64,
    pub cache_read_per_mtok: f64,
    pub cache_write_per_mtok: f64,
}

/// PURE: usage × price. `None` when the model has no row in the table.
pub fn cost_usd(u: &Usage, p: Option<&Price>) -> Option<f64> {
    let _ = (u, p);
    todo!("WP-7")
}
