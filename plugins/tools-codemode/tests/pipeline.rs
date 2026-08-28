//! WP-2: an inner call is an ORDINARY call. The four `tools/*` events fire for it, a
//! `tools/pre-execute` deny reaches the program as a rejected promise and a `denied`
//! `program/result`, a call-cap breach is terminal, and `run` never invents `concludes_wake`.

use crate::support;

use std::sync::Arc;

use bough_plugin_tools::{
    FailureClass, PostExecute, PreExecute, ToolsPostExecute, ToolsPreExecute, ToolsResult,
};
use parking_lot::Mutex;
use support::{config, harness, spec, Echo};

fn echo(concludes: bool) -> Arc<dyn bough_plugin_tools::Tool> {
    Arc::new(Echo { concludes })
}

#[tokio::test]
async fn the_four_tools_events_fire_for_every_inner_call() {
    let h = harness(vec![spec("echo", echo(false))], config()).await;
    let seen: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    let pre = seen.clone();
    h.ctx
        .on_waterfall::<ToolsPreExecute, _, _>(move |p: PreExecute, next| {
            let seen = pre.clone();
            async move {
                seen.lock().push("pre");
                next.run(p).await
            }
        })
        .await
        .unwrap();
    let exec = seen.clone();
    h.ctx
        .on_waterfall::<bough_plugin_tools::ToolsExecute, _, _>(move |e, next| {
            let seen = exec.clone();
            async move {
                seen.lock().push("execute");
                next.run(e).await
            }
        })
        .await
        .unwrap();
    let post = seen.clone();
    h.ctx
        .on_waterfall::<ToolsPostExecute, _, _>(move |p: PostExecute, next| {
            let seen = post.clone();
            async move {
                seen.lock().push("post");
                next.run(p).await
            }
        })
        .await
        .unwrap();
    let res = seen.clone();
    h.ctx
        .on::<ToolsResult, _, _>(move |_| {
            let seen = res.clone();
            async move {
                seen.lock().push("result");
            }
        })
        .await
        .unwrap();

    h.program("call echo [{\"a\":1}]\ncall echo [{\"a\":2}]")
        .await
        .expect("the program ran");

    let seen = seen.lock().clone();
    for stage in ["pre", "execute", "post", "result"] {
        assert_eq!(
            seen.iter().filter(|s| **s == stage).count(),
            2,
            "`{stage}` must fire once per inner call: {seen:?}"
        );
    }
}

#[tokio::test]
async fn a_pre_execute_deny_rejects_the_promise_and_lands_a_denied_program_result() {
    let h = harness(vec![spec("echo", echo(false))], config()).await;
    h.ctx
        .on_waterfall::<ToolsPreExecute, _, _>(|mut pre: PreExecute, next| async move {
            pre.deny("policy says no");
            next.run(pre).await
        })
        .await
        .unwrap();

    let out = h
        .program("call echo []")
        .await
        .expect("a denied inner call is not a failed program");
    assert!(
        out.content.contains("err Denied"),
        "the program saw a rejection carrying the kind: {:?}",
        out.content
    );

    let results = h.steps("program/result").await;
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].body["outcome"], serde_json::json!("denied"));
}

#[tokio::test]
async fn a_cap_breach_lands_a_program_error_step_and_a_failed_tool_result() {
    let mut cfg = config();
    cfg.max_calls_per_program = 1;
    let h = harness(vec![spec("echo", echo(false))], cfg).await;

    let failure = h
        .program("call echo []\ncall echo []")
        .await
        .expect_err("a program past its call budget must fail the round");
    assert_eq!(failure.kind, FailureClass::Blocked);
    assert!(
        failure.message.contains("more than 1 tool calls"),
        "{failure:?}"
    );

    let errors = h.steps("program/error").await;
    assert_eq!(errors.len(), 1, "the breach is one terminal error step");
    assert_eq!(
        h.steps("program/call").await.len(),
        1,
        "the call that was refused by the cap never became a step"
    );
}

#[tokio::test]
async fn run_never_reports_concludes_wake_unless_an_inner_result_did() {
    let quiet = harness(vec![spec("echo", echo(false))], config()).await;
    let out = quiet.program("call echo []").await.unwrap();
    assert!(!out.concludes_wake);

    let ending = harness(vec![spec("ask", echo(true))], config()).await;
    let out = ending.program("call ask []").await.unwrap();
    assert!(
        out.concludes_wake,
        "an inner result that concludes the wake carries through the program"
    );
}

#[tokio::test]
async fn a_program_that_cannot_parse_never_reaches_the_sandbox() {
    let h = harness(vec![spec("echo", echo(false))], config()).await;
    let failure = h
        .program("!!syntax")
        .await
        .expect_err("a syntax error fails the round");
    assert_eq!(failure.kind, FailureClass::Error);
    assert_eq!(h.steps("program/error").await.len(), 1);
    assert!(h.steps("program/call").await.is_empty());
}

/// `docs/codemode-merge-notes.md` §9, end to end: a `bash` whose schema is `{command, cwd}` — the
/// only `bash` the tree registers — is CALLABLE from the sandbox with `tags_required` on, because
/// the tag argument is code mode's and never reaches the tool.
#[tokio::test]
async fn a_tagged_bash_call_reaches_a_tool_that_declares_no_tags() {
    let mut cfg = config();
    cfg.tags_required = true;
    let mut bash = spec("bash", echo(false));
    bash.input_schema = schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "properties": {"command": {"type": "string"}, "cwd": {"type": "string"}},
        "required": ["command"]
    }))
    .unwrap();
    let h = harness(vec![bash], cfg).await;

    let out = h
        .program("call bash [\"echo hi\", \"echo:probe:demo\"]")
        .await
        .expect("the program runs");
    assert!(
        out.content.contains("bash said"),
        "the tagged call was refused: {}",
        out.content
    );
    assert!(
        !out.content.contains("needs 3–5 tags"),
        "the tag rule fired on a tagged call: {}",
        out.content
    );

    let calls = h.steps("program/call").await;
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        calls[0].body["tags"],
        serde_json::json!(["echo", "probe", "demo"]),
        "the tags are on the step"
    );
    assert_eq!(
        calls[0].body["args"],
        serde_json::json!({"command": "echo hi"}),
        "the tags must not reach the tool as `cwd`"
    );
}

/// The other half of the same rule: an UNtagged shell call is still refused, and the refusal is a
/// rejected promise rather than a step.
#[tokio::test]
async fn an_untagged_bash_call_is_still_refused_and_lands_no_step() {
    let mut cfg = config();
    cfg.tags_required = true;
    let mut bash = spec("bash", echo(false));
    bash.input_schema = schemars::Schema::try_from(serde_json::json!({
        "type": "object",
        "properties": {"command": {"type": "string"}},
        "required": ["command"]
    }))
    .unwrap();
    let h = harness(vec![bash], cfg).await;

    let out = h
        .program("call bash [\"echo hi\"]")
        .await
        .expect("the program runs");
    assert!(
        out.content.contains("needs 3–5 tags"),
        "an untagged call must be refused: {}",
        out.content
    );
    assert!(
        h.steps("program/call").await.is_empty(),
        "a leg that never ran is not a call"
    );
}
