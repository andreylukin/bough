//! Invariant under test (V10): retry lives HERE and nowhere else (P2-D5). A retryable failure
//! returns `Retry` WITHOUT calling `next()`; a non-retryable one delegates and the failure stays
//! terminal for the wake; and attempts are BOUNDED, so `RequestErrorCall::attempt` is a true
//! count rather than a hopeful one.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{AgentName, TrajId, WakeId};
use bough_plugin_llm::{
    AdapterName, AgentRequestError, CallConfig, FailureKind, LlmFailure, LlmRequest, Recovery,
    RequestErrorCall, RequestFacts, WakeKind,
};
use bough_plugin_llm_retry::{apply_decision, decide, RetryConfig};

fn cfg(max_attempts: u32, retry_on: Vec<FailureKind>) -> RetryConfig {
    RetryConfig {
        max_attempts,
        min_delay_ms: 1,
        max_delay_ms: 10,
        jitter: false,
        retry_on,
    }
}

fn facts() -> Arc<RequestFacts> {
    Arc::new(RequestFacts {
        agent: AgentName::new("sol"),
        traj: TrajId::new("t-sol"),
        wake: WakeId::new("w1"),
        wake_kind: WakeKind::Answer,
        step_index: 0,
        answers_andrey: true,
        model_override: None,
        prompt_ver: "p1".into(),
        composition: "fp".into(),
    })
}

fn request() -> Arc<LlmRequest> {
    Arc::new(LlmRequest {
        projection_digest: None,
        model: "claude-haiku-4-5-20251001".into(),
        system: None,
        system_volatile: None,
        messages: vec![],
        tools: vec![],
        call: CallConfig {
            model: "claude-haiku-4-5-20251001".into(),
            max_tokens: 64,
            effort: None,
            tool_choice_none: false,
            meta: Default::default(),
        },
    })
}

fn call(kind: FailureKind, retryable: bool, attempt: u32) -> RequestErrorCall {
    RequestErrorCall {
        facts: facts(),
        request: request(),
        failure: LlmFailure {
            kind,
            message: "boom".into(),
            retryable,
            status: None,
            adapter: AdapterName::new("llm-anthropic"),
        },
        attempt,
        recovery: Recovery::Terminal,
    }
}

/// Mount the listener and one inner listener that COUNTS delegations, then dispatch.
async fn through_the_chain(cfg: RetryConfig, c: RequestErrorCall) -> (RequestErrorCall, u32) {
    let ctx = Context::root(KernelCore::new());
    let cfg = Arc::new(cfg);
    ctx.on_waterfall::<AgentRequestError, _, _>(move |mut call: RequestErrorCall, next| {
        let cfg = cfg.clone();
        async move {
            if apply_decision(&cfg, &mut call) {
                return call;
            }
            next.run(call).await
        }
    })
    .await
    .expect("listener registers");

    let inner = Arc::new(AtomicU32::new(0));
    let seen = inner.clone();
    ctx.on_waterfall::<AgentRequestError, _, _>(move |call: RequestErrorCall, next| {
        let seen = seen.clone();
        async move {
            seen.fetch_add(1, Ordering::SeqCst);
            next.run(call).await
        }
    })
    .await
    .expect("listener registers");

    let out = ctx.waterfall::<AgentRequestError>(c).await;
    let delegations = inner.load(Ordering::SeqCst);
    (out, delegations)
}

#[tokio::test]
async fn a_retryable_failure_is_retried_without_next() {
    let (out, delegations) = through_the_chain(
        cfg(3, vec![FailureKind::Overloaded]),
        call(FailureKind::Overloaded, true, 1),
    )
    .await;
    match out.recovery {
        Recovery::Retry { after } => {
            assert!(after > Duration::ZERO, "a retry waits");
        }
        other => panic!("expected a retry, got {other:?}"),
    }
    assert_eq!(
        delegations, 0,
        "a listener that owns recovery returns WITHOUT calling next()"
    );
}

#[tokio::test]
async fn the_default_leaves_the_failure_terminal_for_the_wake() {
    // Not retryable at all: an auth failure will still be an auth failure in fifteen seconds.
    let (out, delegations) = through_the_chain(
        cfg(5, vec![FailureKind::Auth, FailureKind::Overloaded]),
        call(FailureKind::Auth, false, 1),
    )
    .await;
    assert_eq!(out.recovery, Recovery::Terminal);
    assert_eq!(delegations, 1, "a non-retryable failure DELEGATES");

    // Retryable, but of a kind this deployment did not list.
    let (out, delegations) = through_the_chain(
        cfg(5, vec![FailureKind::Overloaded]),
        call(FailureKind::RateLimit, true, 1),
    )
    .await;
    assert_eq!(out.recovery, Recovery::Terminal);
    assert_eq!(delegations, 1);
}

#[tokio::test]
async fn attempts_are_bounded() {
    let c = cfg(3, vec![FailureKind::Transport]);
    for attempt in 1..3 {
        assert!(
            decide(&c, &call(FailureKind::Transport, true, attempt)).is_some(),
            "attempt {attempt} of 3 still has attempts left"
        );
    }
    for attempt in 3..8 {
        assert_eq!(
            decide(&c, &call(FailureKind::Transport, true, attempt)),
            None,
            "attempt {attempt} is past max_attempts = 3"
        );
    }
    // `max_attempts: 1` neuters the row without unmounting it.
    let off = cfg(1, vec![FailureKind::Transport]);
    assert_eq!(decide(&off, &call(FailureKind::Transport, true, 1)), None);

    // And through the chain, the bound means DELEGATION, not a silent stall.
    let (out, delegations) = through_the_chain(c, call(FailureKind::Transport, true, 3)).await;
    assert_eq!(out.recovery, Recovery::Terminal);
    assert_eq!(delegations, 1);
}

#[tokio::test]
async fn the_delay_stays_inside_the_configured_window() {
    let c = RetryConfig {
        max_attempts: 8,
        min_delay_ms: 10,
        max_delay_ms: 40,
        jitter: false,
        retry_on: vec![FailureKind::Transport],
    };
    for attempt in 1..8 {
        let d = decide(&c, &call(FailureKind::Transport, true, attempt))
            .unwrap_or_else(|| panic!("attempt {attempt} should retry"));
        assert!(
            d >= Duration::from_millis(10) && d <= Duration::from_millis(40),
            "attempt {attempt} waited {d:?}, outside [min_delay_ms, max_delay_ms]"
        );
    }
}
