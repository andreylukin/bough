//! Invariant (§9): the guarded pipeline is the ONLY way a tool runs. `tools/pre-execute` decides
//! (monotonically), `tools/execute` wraps dispatch (cancellation only; deadlines `min`),
//! `tools/post-execute` may reshape the result, `tools/result` observes it. Concurrency-safe
//! calls DISPATCH in parallel; everything else forms a barrier — and the results the model sees
//! are in the model's call order regardless.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bough_kernel::Context;
use chrono::Utc;
use tokio_util::sync::CancellationToken;

use crate::pipeline::{
    Decision, Execution, PostExecute, PreExecute, ToolsExecute, ToolsPostExecute, ToolsPreExecute,
    ToolsResult,
};
use crate::tool::{FailureClass, Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome, ToolResult};
use crate::{ApprovalOutcome, ToolsHandle};

/// One call, resolved: the call itself, the tool if it is visible in the caller's scope, and
/// whether `is_concurrency_safe(args)` said EXACTLY `true`.
type Planned = (ToolCall, Option<Arc<dyn Tool>>, bool);

/// One run of the pipeline over the model's calls, in the model's order.
pub(crate) async fn execute(
    handle: &ToolsHandle,
    ctx: &Context,
    calls: Vec<ToolCall>,
    cancel: CancellationToken,
) -> Vec<ToolResult> {
    let max_parallel = handle.0.max_parallel;
    let default_deadline = Duration::from_millis(handle.0.default_deadline_ms);

    // Resolve first, so the barrier layout is decided before anything runs.
    let mut planned: Vec<Planned> = Vec::new();
    for call in calls {
        let resolved = handle.resolve(&call.agent, &call.name).ok();
        let safe = resolved
            .as_ref()
            .map(|t| t.is_concurrency_safe(&call.args))
            .unwrap_or(false);
        planned.push((call, resolved, safe));
    }

    let mut results: Vec<ToolResult> = Vec::with_capacity(planned.len());
    let mut i = 0usize;
    while i < planned.len() {
        // A batch is a run of concurrency-safe calls; anything else is a batch of one and so a
        // barrier for its neighbours.
        // `max_parallel` is clamped at construction, but a batch of zero would spin, so the
        // floor is restated where the loop depends on it.
        let max_parallel = max_parallel.max(1);
        let mut end = i;
        if planned[i].2 {
            while end < planned.len() && planned[end].2 && end - i < max_parallel {
                end += 1;
            }
        } else {
            end = i + 1;
        }
        let batch = &planned[i..end];
        let futs = batch.iter().map(|(call, tool, _)| {
            run_one(
                handle,
                ctx,
                call.clone(),
                tool.clone(),
                default_deadline,
                cancel.clone(),
            )
        });
        let mut batch_results = futures::future::join_all(futs).await;
        results.append(&mut batch_results);
        i = end;
    }

    // Durable results stay MODEL-ORDERED: the observe-only event fires in call order, after the
    // batch, never in completion order.
    for r in &results {
        ctx.emit::<ToolsResult>(Arc::new(r.clone()));
    }
    results
}

async fn run_one(
    handle: &ToolsHandle,
    ctx: &Context,
    call: ToolCall,
    tool: Option<Arc<dyn Tool>>,
    default_deadline: Duration,
    caller_cancel: CancellationToken,
) -> ToolResult {
    let started_at = Utc::now();
    let call = Arc::new(call);

    // ---- the guard (§9): a tool absent from the scope refuses execution there ----------------
    let Some(tool) = tool else {
        return post(
            ctx,
            &call,
            failure(
                &call,
                started_at,
                ToolFailure {
                    kind: FailureClass::NotFound,
                    message: format!(
                        "no tool named `{}` is available to agent `{}`",
                        call.name, call.agent
                    ),
                },
            ),
        )
        .await;
    };

    let pre = ctx
        .waterfall::<ToolsPreExecute>(PreExecute::new(call.clone(), call.agent.clone()))
        .await;
    let denial: Option<String> = match pre.decision().clone() {
        Decision::Allow => None,
        Decision::Deny { reason } => Some(reason),
        Decision::Ask { reason } => match handle.approval() {
            // `ask` is serviced by `ctx.approval` when mounted and DEGRADES TO DENY otherwise.
            Some(approver) => match approver.0.ask(&call, &reason).await {
                ApprovalOutcome::Allow => None,
                ApprovalOutcome::Deny => Some(reason),
            },
            None => Some(format!("{reason} (no approver is mounted)")),
        },
    };
    if let Some(reason) = denial {
        return post(
            ctx,
            &call,
            failure(
                &call,
                started_at,
                ToolFailure {
                    kind: FailureClass::Denied,
                    message: reason,
                },
            ),
        )
        .await;
    }

    // ---- around-dispatch (§9, P2-D13) --------------------------------------------------------
    // A CHILD of the caller's signal: cancelling the wake cancels every tool it has in flight,
    // and a `tools/execute` wrapper may still narrow it further for one call.
    let cancel = caller_cancel.child_token();
    let deadline = Instant::now() + default_deadline;
    let digest_before = call.digest();
    let wrapped = ctx
        .waterfall::<ToolsExecute>(Execution {
            call: call.clone(),
            cancel: cancel.clone(),
            deadline: Some(deadline),
            outcome: None,
        })
        .await;
    // A wrapper may replace ONLY the cancellation signal. An edited call is ignored and logged.
    let call = if wrapped.call.digest() == digest_before {
        wrapped.call.clone()
    } else {
        tracing::warn!(
            tool = %call.name,
            "a tools/execute wrapper edited the call; the edit is ignored (§9 offers no input rewrite)"
        );
        call
    };
    // Deadlines WRAP, never lengthen.
    let deadline = match wrapped.deadline {
        Some(d) => d.min(deadline),
        None => deadline,
    };
    let cancel = wrapped.cancel.clone();

    let outcome = match wrapped.outcome {
        Some(o) => o,
        None => {
            let cx = ToolCx {
                ctx: ctx.clone(),
                cancel: cancel.clone(),
                deadline: Some(deadline),
                initiator: None,
            };
            let fut = tool.call(call.clone(), cx);
            tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(ToolFailure {
                    kind: FailureClass::Cancelled,
                    message: format!("`{}` was cancelled", call.name),
                }),
                r = tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), fut) => match r {
                    Ok(r) => r,
                    Err(_) => Err(ToolFailure {
                        kind: FailureClass::Timeout,
                        message: format!("`{}` exceeded its deadline", call.name),
                    }),
                },
            }
        }
    };

    let result = match outcome {
        Ok(o) => ok(&call, started_at, o),
        Err(f) => failure(&call, started_at, f),
    };
    post(ctx, &call, result).await
}

fn ok(call: &Arc<ToolCall>, started_at: chrono::DateTime<Utc>, o: ToolOutcome) -> ToolResult {
    ToolResult {
        call: call.id.clone(),
        name: call.name.clone(),
        ok: true,
        content: o.content,
        value: o.value,
        attached: vec![],
        cites: o.cites,
        concludes_wake: o.concludes_wake,
        failure: None,
        started_at,
        ended_at: Utc::now(),
    }
}

fn failure(call: &Arc<ToolCall>, started_at: chrono::DateTime<Utc>, f: ToolFailure) -> ToolResult {
    ToolResult {
        call: call.id.clone(),
        name: call.name.clone(),
        ok: false,
        content: f.message.clone(),
        value: None,
        attached: vec![],
        cites: vec![],
        concludes_wake: false,
        failure: Some(f),
        started_at,
        ended_at: Utc::now(),
    }
}

async fn post(ctx: &Context, call: &Arc<ToolCall>, result: ToolResult) -> ToolResult {
    let out = ctx
        .waterfall::<ToolsPostExecute>(PostExecute::new(call.clone(), result))
        .await;
    out.result().clone()
}
