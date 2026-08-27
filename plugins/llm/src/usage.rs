//! Invariant: what a model round COST is a durable step, not a runtime counter (phase ux1 §2.10,
//! M24). It is owned by this seam because this seam owns model-call vocabulary, and it is durable
//! because a cost that vanishes on relaunch is a number the surface cannot honestly show.

/// `usage/round` — Thought, ignorable. One per model round that reported usage.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct UsageRound {
    pub step_index: u32,
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    #[serde(default)]
    pub cache_read_tokens: Option<i64>,
    #[serde(default)]
    pub cache_write_tokens: Option<i64>,
    /// The provider's number when it gives one, else computed from `model-policy.prices`.
    /// `None` means UNKNOWN and must render as `—`, never as `0.0`.
    #[serde(default)]
    pub cost_usd: Option<f64>,
}

/// The step type name.
pub const USAGE_ROUND: &str = "usage/round";
