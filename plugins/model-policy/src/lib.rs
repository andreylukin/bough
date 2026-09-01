//! Invariant (§12): the model a wake runs on is decided HERE, by a PREPEND listener on
//! `agent/request`, and nowhere else. Anything answering Andrey gets `interactive` and cannot be
//! overridden; everything unattended gets `unattended`, or the agent's `model_override` if it has one.
//! Both names are config fields, so swapping the pair is a patch and never a code change.
//!
//! For this build both are `claude-haiku-4-5-20251001` (Andrey's choice for the testing period).

pub mod invariant;
pub mod price;

pub use price::{cost_usd, Price};

use std::sync::Arc;

use bough_kernel::{Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_ledger::{Append, Class, Ledger, StepType, TrajId, WakeId};
use bough_plugin_llm::{Chunk, LlmStreamEvent, RequestCall, StreamCall, UsageRound, USAGE_ROUND};
use futures::StreamExt;
use parking_lot::Mutex;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "model-policy";

/// The row's config: the two model names §12 names.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PolicyConfig {
    /// The model for any wake ANSWERING ANDREY. Not overridable (§12). (Renamed from `sol`
    /// 2026-08-31 — the old spelling collided with a vendor's model codename; the alias keeps
    /// old patches composing.)
    #[serde(alias = "sol")]
    pub interactive: String,
    /// The model for unattended work. `agents.model_override` applies to this one only.
    /// (Renamed from `terra`, same day, same alias rule.)
    #[serde(alias = "terra")]
    pub unattended: String,
    /// Per-model prices, keyed by the model name the provider reports (phase ux1 §2.10). A model
    /// with no row here has an UNKNOWN cost and the status line shows `—` for it — never `$0.00`.
    #[serde(default)]
    pub prices: std::collections::BTreeMap<String, price::Price>,
}

/// The decision, as a pure function of the config and the request's facts — no waterfall, no
/// clock, so V6's four cases are ordinary unit tests.
pub fn choose(cfg: &PolicyConfig, call: &RequestCall) -> String {
    if call.facts.answers_andrey {
        // §12: the interactive model is not overridable: `model_override` is read and
        // DISCARDED, not consulted.
        return cfg.interactive.clone();
    }
    call.facts
        .model_override
        .clone()
        .unwrap_or_else(|| cfg.unattended.clone())
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

/// Where a `usage/round` belongs. A model round carries no coordinates of its own — `LlmRequest`
/// is a request, not a trajectory — so the attribution comes from the `request/header` this row
/// already watches, which `agent-loop` appends immediately before the stream opens.
#[derive(Clone, Debug, PartialEq)]
pub struct Attribution {
    pub traj: TrajId,
    pub wake: WakeId,
    pub step_index: u32,
}

/// The newest header per MODEL. Keyed by model rather than kept as a single slot so two agents
/// streaming on different models cannot be attributed to each other; two rounds of the SAME model
/// in flight at once are attributed to the newer header, and that is the honest limit of this
/// join until a stream carries its own wake.
#[derive(Default)]
pub struct Pending(Mutex<std::collections::BTreeMap<String, Attribution>>);

impl Pending {
    pub fn note(&self, model: &str, a: Attribution) {
        self.0.lock().insert(model.to_string(), a);
    }
    pub fn get(&self, model: &str) -> Option<Attribution> {
        self.0.lock().get(model).cloned()
    }
}

/// PURE: the durable body for one reported round.
pub fn usage_round(
    model: &str,
    step_index: u32,
    u: &bough_plugin_llm::Usage,
    price: Option<&Price>,
) -> UsageRound {
    UsageRound {
        step_index,
        model: model.to_string(),
        input_tokens: u.input_tokens,
        output_tokens: u.output_tokens,
        cache_read_tokens: u.cache_read_tokens,
        cache_write_tokens: u.cache_write_tokens,
        cost_usd: cost_usd(u, price),
    }
}

/// The model a stream is for: the request's own name, falling back to the call config's.
fn model_of(call: &StreamCall) -> String {
    if call.request.model.is_empty() {
        call.request.call.model.clone()
    } else {
        call.request.model.clone()
    }
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
        let reject = |detail: String| Err(bough_kernel::ConfigError::Rejected { detail });
        if cfg.interactive.trim().is_empty() || cfg.unattended.trim().is_empty() {
            return reject("both `interactive` and `unattended` must name a model".to_string());
        }
        // The price table is self-contained data, so §0.5's pure-validator rule applies: a
        // negative, NaN or absurd rate would flow through `price::cost_usd` into a DURABLE
        // `usage/round.cost_usd` and onto the status line, where `status::money` would render a
        // negative running total quite happily. A price is wrong once, at load, or never.
        for (model, p) in cfg.prices.iter() {
            if model.trim().is_empty() {
                return reject("a price row must name a model".to_string());
            }
            for (what, v) in [
                ("input_per_mtok", p.input_per_mtok),
                ("output_per_mtok", p.output_per_mtok),
                ("cache_read_per_mtok", p.cache_read_per_mtok),
                ("cache_write_per_mtok", p.cache_write_per_mtok),
            ] {
                if !v.is_finite() || v < 0.0 {
                    return reject(format!(
                        "prices[{model:?}].{what} is {v}: a price is a finite, non-negative number \
                         of dollars per million tokens"
                    ));
                }
                // A sanity ceiling. Nothing on the market is within two orders of magnitude of
                // this, and a fat-fingered patch that is silently 1000x wrong is a durable lie.
                if v > 10_000.0 {
                    return reject(format!(
                        "prices[{model:?}].{what} is {v} $/Mtok, which is not a price anyone \
                         charges; the units are dollars per MILLION tokens"
                    ));
                }
            }
        }
        Ok(())
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        invariant::set_configured(&cfg.interactive, &cfg.unattended);
        let entry = ctx.entry_id().clone();
        let ledger = ctx
            .get::<Ledger>()
            .map_err(|e| PluginError::new(entry.clone(), e))?;
        // `llm` OWNS the `usage/round` vocabulary; this row installs it, because this row is the
        // one that injects a ledger and appends the step (phase ux1 §2.10).
        ledger
            .declare_step_types(&ctx, bough_plugin_llm::usage_step_types())
            .await?;
        let pending: Arc<Pending> = Arc::new(Pending::default());
        let cfg2 = cfg.clone();

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
        let (p, prices) = (pending.clone(), cfg2.prices.clone());
        ctx.on::<bough_plugin_ledger::LedgerStep, _, _>(move |step| {
            let p = p.clone();
            async move {
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
                // Where the round that is about to open belongs (phase ux1 §2.10, M24).
                p.note(
                    model,
                    Attribution {
                        traj: step.traj.clone(),
                        wake: step.wake.clone(),
                        step_index: step_index_of(&step),
                    },
                );
            }
        })
        .await?;

        // The `usage/round` WRITER. One task, owned by this row through `effect_spawn`, fed by a
        // channel the stream tee sends into. It used to be a bare `tokio::spawn` per round: a
        // task owned by nothing, holding a `LedgerHandle` cloned out of this row's committed
        // view, neither cancelled nor awaited on unload, and free to race `ledger.sqlite`'s
        // `checkpoint()`-then-`retire()` disposer on shutdown (the M28 class). It is registered
        // BEFORE the tee so LIFO recovery disposes it AFTER the tee has stopped producing.
        let (usage_tx, mut usage_rx) =
            tokio::sync::mpsc::unbounded_channel::<(Attribution, UsageRound)>();
        let writer_ledger = ledger.clone();
        ctx.effect_spawn(move |_e| async move {
            while let Some((a, body)) = usage_rx.recv().await {
                if let Err(e) = writer_ledger
                    .0
                    .append(Append {
                        traj: a.traj,
                        wake: a.wake,
                        kind: StepType::new(USAGE_ROUND),
                        class: Class::Thought,
                        body: serde_json::to_value(&body).unwrap_or(serde_json::Value::Null),
                        cites: vec![],
                        at: chrono::Utc::now(),
                        id: None,
                    })
                    .await
                {
                    // A cost line that failed to write is a missing number on the status line,
                    // never a failed model round — but it is no longer SILENT, which is what made
                    // M24's "$— forever" indistinguishable from "nothing has cost anything yet".
                    tracing::warn!("model-policy: usage/round append failed: {e}");
                }
            }
            Ok(())
        });

        // The usage tee: an OBSERVER on `llm/stream`, exactly the shape `tui-focus`'s text tee
        // has — `next` runs first, nothing is replaced, nothing is short-circuited, and every
        // chunk passes through byte-identical.
        let (p, l, prices) = (pending.clone(), usage_tx, Arc::new(prices));
        ctx.on_waterfall::<LlmStreamEvent, _, _>(move |call, next| {
            let (p, l, prices) = (p.clone(), l.clone(), prices.clone());
            async move {
                let filled = next.run(call).await;
                let model = model_of(&filled);
                let Some(stream) = filled.stream.take() else {
                    // Nothing filled the slot; a wrapper over nothing would turn a downstream
                    // `Chunk::Failed` into a hang.
                    return filled;
                };
                filled.stream.put(
                    stream
                        .map(move |chunk| {
                            if let Chunk::Usage(u) = &chunk {
                                if let Some(a) = p.get(&model) {
                                    let body =
                                        usage_round(&model, a.step_index, u, prices.get(&model));
                                    // The writer owns the append; a closed channel means the row
                                    // is going away and there is nothing to write into.
                                    let _ = l.send((a, body));
                                }
                            }
                            chunk
                        })
                        .boxed(),
                );
                filled
            }
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::answer_wakes_get_sol()]
    }
}

bough_kernel::register_plugin!(ModelPolicyPlugin);

#[cfg(test)]
mod usage_tests {
    use super::*;

    fn usage() -> bough_plugin_llm::Usage {
        bough_plugin_llm::Usage {
            input_tokens: 2_000_000,
            output_tokens: 0,
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
    fn a_round_carries_the_model_the_index_and_a_priced_cost() {
        let r = usage_round("haiku", 7, &usage(), Some(&price()));
        assert_eq!(r.model, "haiku");
        assert_eq!(r.step_index, 7);
        assert_eq!(r.input_tokens, 2_000_000);
        assert_eq!(r.cost_usd, Some(2.0));
    }

    #[test]
    fn an_unpriced_model_reports_an_unknown_cost() {
        let r = usage_round("nobody-prices-this", 0, &usage(), None);
        assert_eq!(r.cost_usd, None, "unknown is never 0.0");
    }

    #[test]
    fn pending_attributes_per_model_and_the_newest_header_wins() {
        let p = Pending::default();
        assert_eq!(p.get("a"), None);
        let a1 = Attribution {
            traj: TrajId::new("t1"),
            wake: WakeId::new("w1"),
            step_index: 1,
        };
        let a2 = Attribution {
            traj: TrajId::new("t2"),
            wake: WakeId::new("w2"),
            step_index: 2,
        };
        p.note("a", a1);
        p.note("b", a2.clone());
        assert_eq!(p.get("a").unwrap().step_index, 1, "models do not cross");
        assert_eq!(p.get("b").unwrap(), a2);
    }
}
