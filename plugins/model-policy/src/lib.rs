//! Invariant (§12): the model a wake runs on is decided HERE, by a PREPEND listener on
//! `agent/request`, and nowhere else. Anything answering Andrey gets `sol` and cannot be
//! overridden; everything unattended gets `terra`, or the agent's `model_override` if it has one.
//! Both names are config fields, so swapping the pair is a patch and never a code change.
//!
//! For this build both are `claude-haiku-4-5-20251001` (Andrey's choice for the testing period).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_llm::RequestCall;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "model-policy";

/// The row's config: the two model names §12 names.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// The model for any wake ANSWERING ANDREY. Not overridable (§12).
    pub sol: String,
    /// The model for unattended work. `agents.model_override` applies to this one only.
    pub terra: String,
}

/// The decision, as a pure function of the config and the request's facts — no waterfall, no
/// clock, so V6's four cases are ordinary unit tests.
///
/// WP-5.
pub fn choose(_cfg: &PolicyConfig, _call: &RequestCall) -> String {
    todo!("WP-5: answers_andrey => sol (override ignored), else model_override or terra")
}

/// The consumer row.
pub struct ModelPolicyPlugin;

#[async_trait::async_trait]
impl Plugin for ModelPolicyPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = PolicyConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["llm", "ledger"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        if cfg.sol.trim().is_empty() || cfg.terra.trim().is_empty() {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "both `sol` and `terra` must name a model".to_string(),
            });
        }
        Ok(())
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-5: a PREPEND listener on agent/request that sets call.model = choose(..)")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::answer_wakes_get_sol()]
    }
}

bough_kernel::register_plugin!(ModelPolicyPlugin);
