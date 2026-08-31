//! V5 — the tools pipeline (§9). Nine cases: the monotone guard, `ask` with and without an
//! approver, the three post-execute mutations, `block`, and the concurrency-safe / barrier
//! dispatcher with model-ordered results.

use crate::support;

use std::sync::Arc;
use std::time::Duration;

use bough_plugin_tools::{
    ApprovalHandle, ApprovalOutcome, Approver, AttachedContext, FailureClass, PostExecute,
    PreExecute, ToolCall, ToolsHandle, ToolsPostExecute, ToolsPreExecute,
};
use parking_lot::Mutex;
use support::{agent, call, ctx, registry_with, spec, Stub};

fn stub(
    safe: bool,
    delay_ms: u64,
    log: Arc<Mutex<Vec<String>>>,
) -> Arc<dyn bough_plugin_tools::Tool> {
    Arc::new(Stub {
        safe,
        delay: Duration::from_millis(delay_ms),
        log,
    })
}

async fn one_tool(ctx: &bough_kernel::Context) -> ToolsHandle {
    let log = Arc::new(Mutex::new(Vec::new()));
    registry_with(ctx, vec![spec("echo", stub(true, 0, log))]).await
}

// ---- the guard ----------------------------------------------------------------------------

#[tokio::test]
async fn a_denial_cannot_be_re_allowed_by_a_later_listener() {
    let ctx = ctx();
    let tools = one_tool(&ctx).await;
    // The first listener denies; the second tries to soften it. `Decision` has no widening
    // constructor (P2-D12), so the best the second can do is `ask` — and it must not take.
    ctx.on_waterfall::<ToolsPreExecute, _, _>(|mut pre: PreExecute, next| async move {
        pre.deny("first says no");
        next.run(pre).await
    })
    .await
    .unwrap();
    ctx.on_waterfall::<ToolsPreExecute, _, _>(|mut pre: PreExecute, next| async move {
        pre.ask("second would rather ask");
        next.run(pre).await
    })
    .await
    .unwrap();

    let out = tools.execute(&ctx, vec![call("echo", "a")]).await;
    assert!(!out[0].ok);
    assert_eq!(out[0].failure.as_ref().unwrap().kind, FailureClass::Denied);
    assert!(
        out[0]
            .failure
            .as_ref()
            .unwrap()
            .message
            .contains("first says no"),
        "the FIRST denial's reason survives: {:?}",
        out[0].failure
    );
}

#[tokio::test]
async fn ask_degrades_to_deny_without_approval() {
    let ctx = ctx();
    let tools = one_tool(&ctx).await;
    assert!(tools.approval().is_none(), "Phase 2 mounts no approver");
    ctx.on_waterfall::<ToolsPreExecute, _, _>(|mut pre: PreExecute, next| async move {
        pre.ask("needs a human");
        next.run(pre).await
    })
    .await
    .unwrap();

    let out = tools.execute(&ctx, vec![call("echo", "a")]).await;
    assert!(!out[0].ok);
    assert_eq!(out[0].failure.as_ref().unwrap().kind, FailureClass::Denied);
    assert!(out[0]
        .failure
        .as_ref()
        .unwrap()
        .message
        .contains("needs a human"));
}

struct Yes;
#[async_trait::async_trait]
impl Approver for Yes {
    async fn ask(&self, _call: &ToolCall, _reason: &str) -> ApprovalOutcome {
        ApprovalOutcome::Allow
    }
}
struct No;
#[async_trait::async_trait]
impl Approver for No {
    async fn ask(&self, _call: &ToolCall, _reason: &str) -> ApprovalOutcome {
        ApprovalOutcome::Deny
    }
}

#[tokio::test]
async fn ask_is_serviced_when_approval_is_mounted() {
    let ctx = ctx();
    let tools = one_tool(&ctx).await;
    tools
        .mount_approval(&ctx, ApprovalHandle(Arc::new(Yes)))
        .await
        .unwrap();
    ctx.on_waterfall::<ToolsPreExecute, _, _>(|mut pre: PreExecute, next| async move {
        pre.ask("needs a human");
        next.run(pre).await
    })
    .await
    .unwrap();

    let out = tools.execute(&ctx, vec![call("echo", "a")]).await;
    assert!(out[0].ok, "an approved ask runs: {:?}", out[0].failure);
    assert_eq!(out[0].content, "a");

    // ...and a refusing approver denies it, so servicing is not the same as granting.
    let tools = one_tool(&ctx).await;
    tools
        .mount_approval(&ctx, ApprovalHandle(Arc::new(No)))
        .await
        .unwrap();
    let out = tools.execute(&ctx, vec![call("echo", "b")]).await;
    assert_eq!(out[0].failure.as_ref().unwrap().kind, FailureClass::Denied);
}

// ---- post-execute -------------------------------------------------------------------------

#[tokio::test]
async fn accept_replaces_content_or_value_never_both() {
    let ctx = ctx();
    let tools = one_tool(&ctx).await;
    ctx.on_waterfall::<ToolsPostExecute, _, _>(|mut post: PostExecute, next| async move {
        post.accept_value(serde_json::json!({ "n": 1 }));
        assert_eq!(post.result().content, "", "accept_value clears the content");
        post.accept_content("replaced".into());
        next.run(post).await
    })
    .await
    .unwrap();

    let out = tools.execute(&ctx, vec![call("echo", "a")]).await;
    assert_eq!(out[0].content, "replaced");
    assert_eq!(out[0].value, None, "accept_content clears the value");
}

#[tokio::test]
async fn accept_may_attach_contexts() {
    let ctx = ctx();
    let tools = one_tool(&ctx).await;
    ctx.on_waterfall::<ToolsPostExecute, _, _>(|mut post: PostExecute, next| async move {
        post.attach(AttachedContext {
            id: "reminder".into(),
            text: "you called this before".into(),
        });
        next.run(post).await
    })
    .await
    .unwrap();

    let out = tools.execute(&ctx, vec![call("echo", "a")]).await;
    assert_eq!(out[0].content, "a", "attach touches neither content...");
    assert_eq!(out[0].value, None, "...nor value");
    assert_eq!(out[0].attached.len(), 1);
    assert_eq!(out[0].attached[0].id, "reminder");
}

#[tokio::test]
async fn block_yields_a_valueless_failure() {
    let ctx = ctx();
    let tools = one_tool(&ctx).await;
    ctx.on_waterfall::<ToolsPostExecute, _, _>(|mut post: PostExecute, next| async move {
        post.accept_value(serde_json::json!({ "n": 1 }));
        post.block("that result may not be used");
        next.run(post).await
    })
    .await
    .unwrap();

    let out = tools.execute(&ctx, vec![call("echo", "a")]).await;
    assert!(!out[0].ok);
    assert_eq!(out[0].value, None, "a blocked result is VALUELESS");
    assert_eq!(out[0].failure.as_ref().unwrap().kind, FailureClass::Blocked);
    assert!(out[0].content.contains("may not be used"));
}

// ---- the dispatcher -----------------------------------------------------------------------

#[tokio::test]
async fn concurrency_safe_calls_dispatch_in_parallel() {
    let ctx = ctx();
    let log = Arc::new(Mutex::new(Vec::new()));
    let tools = registry_with(&ctx, vec![spec("safe", stub(true, 60, log.clone()))]).await;

    tools
        .execute(&ctx, vec![call("safe", "a"), call("safe", "b")])
        .await;

    let seen = log.lock().clone();
    assert_eq!(
        seen,
        vec!["start a", "start b", "end a", "end b"],
        "both starts precede both ends: dispatch OVERLAPPED"
    );
}

#[tokio::test]
async fn an_unsafe_call_forms_an_exclusive_barrier() {
    let ctx = ctx();
    let log = Arc::new(Mutex::new(Vec::new()));
    let tools = registry_with(
        &ctx,
        vec![
            spec("safe", stub(true, 20, log.clone())),
            spec("unsafe", stub(false, 20, log.clone())),
        ],
    )
    .await;

    tools
        .execute(
            &ctx,
            vec![call("safe", "a"), call("unsafe", "x"), call("safe", "b")],
        )
        .await;

    let seen = log.lock().clone();
    assert_eq!(
        seen,
        vec!["start a", "end a", "start x", "end x", "start b", "end b"],
        "the unsafe call is exclusive on both sides"
    );
}

#[tokio::test]
async fn durable_results_stay_model_ordered() {
    let ctx = ctx();
    let log = Arc::new(Mutex::new(Vec::new()));
    // `slow` finishes last but was called first.
    let tools = registry_with(
        &ctx,
        vec![
            spec("slow", stub(true, 80, log.clone())),
            spec("fast", stub(true, 0, log.clone())),
        ],
    )
    .await;

    let out = tools
        .execute(&ctx, vec![call("slow", "a"), call("fast", "b")])
        .await;

    assert_eq!(
        log.lock().clone(),
        vec!["start a", "start b", "end b", "end a"],
        "completion order is the reverse of call order"
    );
    let names: Vec<String> = out.iter().map(|r| r.name.to_string()).collect();
    assert_eq!(names, vec!["slow".to_string(), "fast".to_string()]);
    assert_eq!(out[0].content, "a");
    assert_eq!(out[1].content, "b");
}

// ---- the executor's own refusal -------------------------------------------------------------

#[tokio::test]
async fn a_tool_outside_the_scope_is_refused_by_the_executor() {
    let ctx = ctx();
    let tools = one_tool(&ctx).await;
    let out = tools.execute(&ctx, vec![call("nonexistent", "a")]).await;
    assert_eq!(
        out[0].failure.as_ref().unwrap().kind,
        FailureClass::NotFound
    );
    assert!(out[0]
        .failure
        .as_ref()
        .unwrap()
        .message
        .contains(agent().as_str()));
}
