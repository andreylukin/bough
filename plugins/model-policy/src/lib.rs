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
pub fn choose(cfg: &PolicyConfig, call: &RequestCall) -> String {
    if call.facts.answers_andrey {
        // §12: sol is not overridable. `model_override` is read and DISCARDED, not consulted.
        return cfg.sol.clone();
    }
    call.facts
        .model_override
        .clone()
        .unwrap_or_else(|| cfg.terra.clone())
}

/// Whether the choice actually applied `agents.model_override`. The invariant records this
/// rather than "the facts carried one": a resident agent may have an override and still be
/// messaged by Andrey, and the honest statement is that it never reached the request.
pub fn applied_override(_cfg: &PolicyConfig, call: &RequestCall) -> bool {
    !call.facts.answers_andrey && call.facts.model_override.is_some()
}

/// The step index a `request/header` belongs to: `agent-loop` appends the header inside the step
/// whose `step/start` precedes it, and the header body carries no index of its own, so the join
/// key is the STEP the header's `as_of` sits in. The loop stamps it on the body for exactly this
/// reason; a header without one joins to step 0.
fn step_index_of(step: &bough_plugin_ledger::Step) -> u32 {
    step.body
        .get("step_index")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as u32
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

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        invariant::set_configured(&cfg.sol, &cfg.terra);

        // Per fiber LIFE, not per apply: a reload keeps the `FiberUid`, so this fiber's
        // observations are forgotten when it unloads (§0.3).
        let mine = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(mine));
            Ok(())
        })
        .await?;

        // PREPEND (§12): the policy decides FIRST, so a later listener on the same waterfall is
        // free to refine the call config it was handed. Nothing else in the tree writes
        // `call.model`.
        let opts = bough_kernel::ListenerOpts {
            prepend: true,
            ..Default::default()
        };
        ctx.on_waterfall_with::<bough_plugin_llm::AgentRequest, _, _>(
            opts,
            move |mut value: RequestCall, next| {
                let cfg = cfg.clone();
                async move {
                    value.call.model = choose(&cfg, &value);
                    invariant::record(invariant::Obs {
                        fiber: mine,
                        wake: value.facts.wake.to_string(),
                        step_index: value.facts.step_index,
                        wake_kind: value.facts.wake_kind,
                        answers_andrey: value.facts.answers_andrey,
                        chose: value.call.model.clone(),
                        had_override: applied_override(&cfg, &value),
                    });
                    next.run(value).await
                }
            },
        )
        .await?;

        // The invariant's OTHER half, and the only honest one: the model the ledger says was
        // actually requested. `agent-loop` appends `request/header` after this waterfall, so the
        // call config it records is what a listener downstream of the policy left behind.
        ctx.on::<bough_plugin_ledger::LedgerStep, _, _>(move |step| async move {
            if step.kind.as_str() != "request/header" {
                return;
            }
            let Some(model) = step
                .body
                .get("call")
                .and_then(|c| c.get("model"))
                .and_then(|m| m.as_str())
            else {
                return;
            };
            invariant::record_sent(invariant::SentObs {
                fiber: mine,
                wake: step.wake.to_string(),
                step_index: step_index_of(&step),
                model: model.to_string(),
            });
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::answer_wakes_get_sol()]
    }
}

bough_kernel::register_plugin!(ModelPolicyPlugin);
