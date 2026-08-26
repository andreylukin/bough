//! §12's model policy, as V6 states it: sol for anything answering Andrey, terra for unattended
//! work, `agents.model_override` for unattended work only — and sol is NOT overridable.
//!
//! The decision is a pure function, so these are ordinary unit tests; the last case also drives
//! the real PREPEND listener through a live `agent/request` waterfall, because "the policy
//! decides, and nothing else writes `call.model`" is a statement about the wiring, not about
//! `choose`.

use std::collections::BTreeMap;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{AgentName, TrajId, WakeId};
use bough_plugin_llm::{AgentRequest, CallConfig, RequestCall, RequestFacts, WakeKind};
use bough_plugin_model_policy::{choose, PolicyConfig};

fn cfg() -> PolicyConfig {
    PolicyConfig {
        sol: "sol-model".into(),
        terra: "terra-model".into(),
    }
}

fn call(kind: WakeKind, answers_andrey: bool, model_override: Option<&str>) -> RequestCall {
    RequestCall {
        facts: Arc::new(RequestFacts {
            agent: AgentName::new("sol"),
            traj: TrajId::new("t1"),
            wake: WakeId::new("w1"),
            wake_kind: kind,
            step_index: 0,
            answers_andrey,
            model_override: model_override.map(str::to_string),
            prompt_ver: "p2.1".into(),
            composition: "c".into(),
        }),
        call: CallConfig {
            // Whatever the loop seeded: the policy overwrites it.
            model: "unset".into(),
            max_tokens: 8192,
            effort: None,
            tool_choice_none: false,
            meta: BTreeMap::new(),
        },
    }
}

/// V6 case 1.
#[test]
fn an_answer_wake_gets_sol() {
    assert_eq!(
        choose(&cfg(), &call(WakeKind::Answer, true, None)),
        "sol-model"
    );
}

/// V6 case 2.
#[test]
fn an_unattended_wake_gets_terra() {
    for kind in [
        WakeKind::Drain,
        WakeKind::Scheduled,
        WakeKind::Catchup,
        WakeKind::Task,
    ] {
        assert_eq!(
            choose(&cfg(), &call(kind, false, None)),
            "terra-model",
            "{kind:?} is unattended"
        );
    }
}

/// V6 case 3.
#[test]
fn model_override_applies_to_unattended_only() {
    assert_eq!(
        choose(
            &cfg(),
            &call(WakeKind::Drain, false, Some("some-other-model"))
        ),
        "some-other-model"
    );
    assert_eq!(
        choose(
            &cfg(),
            &call(WakeKind::Answer, true, Some("some-other-model"))
        ),
        "sol-model",
        "an override must not reach a wake that answers Andrey"
    );
}

/// V6 case 4: sol is not overridable — and not by a LATER listener on the same waterfall either,
/// which is why the policy prepends: a listener that runs after it is refining a call the policy
/// already decided, and the recorded decision is still sol.
#[tokio::test]
async fn sol_is_not_overridable() {
    let ctx = Context::root(KernelCore::new());

    // A listener registered BEFORE the policy: since the policy prepends, it still runs second,
    // and the policy's choice is what the chain starts from.
    ctx.on_waterfall::<AgentRequest, _, _>(|mut v: RequestCall, next| async move {
        assert_eq!(
            v.call.model, "sol-model",
            "the policy runs first, whatever the registration order"
        );
        v.call.max_tokens = 1;
        next.run(v).await
    })
    .await
    .expect("a listener registers");

    let opts = bough_kernel::ListenerOpts {
        prepend: true,
        ..Default::default()
    };
    let cfg = Arc::new(cfg());
    ctx.on_waterfall_with::<AgentRequest, _, _>(opts, move |mut v: RequestCall, next| {
        let cfg = cfg.clone();
        async move {
            v.call.model = choose(&cfg, &v);
            next.run(v).await
        }
    })
    .await
    .expect("the policy registers");

    let out = ctx
        .waterfall::<AgentRequest>(call(WakeKind::Answer, true, Some("some-other-model")))
        .await;
    assert_eq!(out.call.model, "sol-model");
    assert_eq!(
        out.call.max_tokens, 1,
        "the rest of the call config is still writable"
    );
}
