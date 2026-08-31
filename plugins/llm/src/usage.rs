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

/// The step type this crate owns, for `declare_step_types`.
///
/// IGNORABLE (§3, P1-D7): a binary that does not know `usage/round` skips those rows on read
/// rather than refusing the trajectory. A cost line is never worth failing a history for.
///
/// DEVIATION from §2.10: the DECLARATION lives here, with the vocabulary, but the row that
/// installs it into the ledger is `model-policy` — `llm` injects no ledger, and giving it one so
/// it could self-declare would invert the dependency the seam is built on.
pub fn step_types() -> Vec<bough_plugin_ledger::StepTypeDef> {
    use bough_plugin_ledger::{ClassRule, StepTypeDef};
    vec![StepTypeDef::of::<UsageRound>(USAGE_ROUND, "llm")
        .class_rule(ClassRule::Thought)
        .ignorable(true)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_round_is_thought_and_ignorable() {
        let defs = step_types();
        assert_eq!(defs.len(), 1);
        assert_eq!(defs[0].name.as_str(), "usage/round");
        assert!(defs[0].ignorable, "a cost line never fails a history");
        assert_eq!(defs[0].class_rule, bough_plugin_ledger::ClassRule::Thought);
    }

    /// An unknown price is UNKNOWN: the body must be able to say so, and `None` must survive a
    /// round trip rather than defaulting to zero.
    fn round(cost: Option<f64>) -> UsageRound {
        UsageRound {
            step_index: 3,
            model: "claude-haiku-4-5-20251001".into(),
            input_tokens: 100,
            output_tokens: 20,
            cache_read_tokens: None,
            cache_write_tokens: None,
            cost_usd: cost,
        }
    }

    #[test]
    fn an_unknown_cost_round_trips_as_none() {
        let v = serde_json::to_value(round(None)).unwrap();
        let back: UsageRound = serde_json::from_value(v).unwrap();
        assert_eq!(back.cost_usd, None);
        let back2: UsageRound =
            serde_json::from_value(serde_json::to_value(round(Some(0.5))).unwrap()).unwrap();
        assert_eq!(back2.cost_usd, Some(0.5));
    }
}
