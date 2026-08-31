//! Invariant: this module IS §5's wake flow, in the drawn order, and nothing else in the tree
//! runs a wake. Every numbered step below is a durable ledger append or a named dispatch; a
//! plugin failure ends the WAKE and not the loop.
//!
//! ```text
//!  1. urgency decides the wake            9. llm.stream(..) through `llm/stream`
//!  2. append `wake/start`                10. tools.execute(..) — the three-stage pipeline
//!  3. cell.claim(..)                     11. append `step/end`
//!  4. waterfall `agent/pre-step`         12. on failure: waterfall `agent/request-error`
//!  5. append `step/start`                13. another step, if tools or next-step input owe one
//!  6. projection.assemble + transcript   14. serial `agent/wake-stopping`, then re-read the inbox
//!  7. append `request/header` on change  15. append `wake/end`
//!  8. waterfall `agent/request`          16. completed only: parallel `agent/wake-end`
//!                                        17. the standing invariant
//! ```
//!
//! DEVIATION from §2.8's numbering, stated plainly: the claim (3) runs BEFORE `wake/start` (2) is
//! appended, because `wake/start.claimed` cannot be filled in before the claim has happened and
//! the ledger is append-only. The claim's own `inbox/spliced` steps carry this wake's id, so the
//! wake is still the first thing the ledger knows about; what moves is one body field, not the
//! order in which the wake becomes durable.

use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_agents::vocabulary::WakeJot;
use bough_plugin_agents::{
    AgentCell, AgentPreStep, AgentRequest, AgentRequestError, AgentWakeEnd, AgentWakeStopping,
    CancelCause, ClaimedMessage, MessageId, PreStep, PreStepDecision, Recovery, RequestCall,
    RequestErrorCall, RequestFacts, Sender, Target, WakeEnded as WakeEndedEvent, WakeStopping,
};
use bough_plugin_ledger::vocabulary::{
    MailClass as VMailClass, MailDelivered, RequestHeader, StepEnd, StepOutcome, WakeEnd,
    WakeEndReason, WakeStart,
};
use bough_plugin_ledger::{
    Append, Cite, Class, LedgerHandle, Ref, Seq, SeqRange, Step, StepQuery, StepType, WakeId,
};
use bough_plugin_llm::{
    CallConfig, Chunk, LlmContentBlock, LlmHandle, LlmMessage, LlmRole, StopReason, WakeKind,
};
use bough_plugin_projection::{AssembleRequest, ProjectionHandle};
use bough_plugin_tools::vocabulary::{ToolCallBody, ToolOutcomeKind, ToolResultBody};
use bough_plugin_tools::{ToolCall, ToolsHandle};
use chrono::Utc;
use futures::StreamExt;

use crate::request::{self, RequestInputs};

/// Dispatch at one agent's scope: untagged listeners plus that agent's own plus its ancestors'.
fn dispatch<'a>(
    ctx: &'a Context,
    scope: &bough_kernel::ScopeKey,
) -> bough_kernel::scope::ScopedDispatch<'a> {
    bough_kernel::scope::scope_target(ctx, scope)
}
use crate::LoopConfig;

/// The seams one wake runs against. Gathered once at attach, so a wake never resolves a service
/// mid-flight and a rebind cannot change what a request was built from halfway through.
#[derive(Clone)]
pub struct LoopDeps {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub projection: ProjectionHandle,
    pub llm: LlmHandle,
    pub tools: ToolsHandle,
    /// The composition fingerprint (§0.5), stamped into every `request/header`.
    pub composition: String,
    pub cfg: Arc<LoopConfig>,
}

/// Why one wake is running and what it claimed.
#[derive(Clone, Debug)]
pub struct WakeSpec {
    pub wake: WakeId,
    pub kind: WakeKind,
    pub urgency: crate::mail::Urgency,
    /// The message whose arrival triggered it, if any.
    pub trigger: Option<MessageId>,
    /// A jot to resume from: the next wake of ANY kind after a preemption (§5).
    pub resume_from: Option<bough_plugin_ledger::StepId>,
    /// Fired when Andrey's message preempts this wake: it gets ONE grace step and closes as
    /// `interrupted` (§5). Distinct from the agent's cancel token, which is a CANCELLATION.
    pub interrupt: tokio_util::sync::CancellationToken,
    /// Set on the first streamed reply token: §5's "started responding" cutoff, which is what
    /// decides whether an arriving message joins this wake or queues behind it.
    pub streamed: Arc<std::sync::atomic::AtomicBool>,
    /// Messages that JOINED this wake (§5, P2-D15): Andrey mail that arrived on the `next-wake`
    /// queue while this answer wake was running and had not yet streamed a token. The wake claims
    /// them at its next STEP boundary, so one answer wake answers both messages rather than a
    /// second one opening behind it.
    pub joined: Arc<parking_lot::Mutex<Vec<MessageId>>>,
}

/// How one wake ended.
#[derive(Clone, Debug, PartialEq)]
pub struct WakeEndedOutcome {
    pub reason: WakeEndReason,
    /// Set when `reason` is `aborted`.
    pub cause: Option<CancelCause>,
    /// The consumed seqs, which the union at §5 is taken over.
    pub consumed: Vec<SeqRange>,
    pub steps: u32,
}

/// Run one wake, start to finish. The whole of §5's diagram lives here.
pub async fn run_wake(cell: &AgentCell, spec: WakeSpec, deps: &LoopDeps) -> WakeEndedOutcome {
    match run_inner(cell, &spec, deps).await {
        Ok(out) => out,
        // §5: a plugin failure ends the WAKE, not the loop. Anything that escapes a stage lands
        // here, the wake closes durably with `error`, and the driver stays up.
        Err(detail) => {
            tracing::warn!(wake = %spec.wake, detail, "wake ended on a failure");
            let out = WakeEndedOutcome {
                reason: WakeEndReason::Error,
                cause: None,
                consumed: Vec::new(),
                steps: 0,
            };
            let _ = close_wake(cell, &spec, deps, &out, Some(detail)).await;
            out
        }
    }
}

async fn run_inner(
    cell: &AgentCell,
    spec: &WakeSpec,
    deps: &LoopDeps,
) -> Result<WakeEndedOutcome, String> {
    let agent = cell.agent().clone();
    let traj = agent.traj().clone();
    let name = agent.name().clone();
    let ctx = agent.ctx().clone();
    // §5's per-agent scope: a wake dispatches AT the agent's scope, so a scoped listener (a
    // lane's own policy) is admitted alongside the global ones, and an unscoped dispatch cannot
    // silently drop it.
    let scope = agent.scope_key().clone();
    let cancel = cell.cancel_token();

    // 3. Claim: a pure DELETION splice. An answer wake claims its trigger only; a drain claims
    //    ordinary seqs only (§5).
    let mut selector = crate::mail::selector_for(spec.kind, Target::NextWake);
    if let (WakeKind::Answer, Some(trigger)) = (spec.kind, spec.trigger.as_ref()) {
        selector = crate::mail::only_the_trigger(selector, trigger);
    }
    let mut claimed: Vec<ClaimedMessage> = cell
        .claim(selector, spec.wake.clone(), Utc::now())
        .await
        .map_err(|e| e.to_string())?;
    // The wake OPENS by claiming both queues: §5 claims "trigger + queued mail" at the start and
    // narrows to `next-step` only BETWEEN steps. A steer that arrived while the agent was idle is
    // the message this wake exists for.
    claimed.extend(claim_next_step(cell, spec).await?);
    let mut consumed = crate::mail::consumed_of(&claimed);

    // 2. wake/start (durable), carrying the urgency, the trigger and what was claimed.
    let trigger_step = claimed.first().map(|c| c.claim_step.clone());
    append(
        deps,
        &traj,
        &spec.wake,
        "wake/start",
        Class::Thought,
        serde_json::to_value(WakeStart {
            urgency: spec.urgency.durable(spec.kind),
            trigger: trigger_step,
            claimed: consumed.clone(),
        })
        .map_err(|e| e.to_string())?,
        vec![],
    )
    .await?;

    // §2's `agent/wake` START moment. It is emitted by BOTH loop Providers, and the agents
    // invariant's "a disposed agent starts no wake" clause is what consumes it; without it that
    // clause was unreachable and a consumer listening for the start got nothing from either
    // driver.
    dispatch(&ctx, &scope).emit::<bough_plugin_agents::AgentWake>(bough_plugin_agents::WakeEvent {
        agent: agent.id().clone(),
        wake: spec.wake.clone(),
        kind: spec.kind,
        phase: bough_plugin_agents::Phase::Start,
    });

    // A preempted wake resumes from its jot: `wake/resumed` is the first step of the next wake of
    // ANY kind, and the jot itself is folded into the request by `transcript::rebuild`.
    if let Some(jot) = &spec.resume_from {
        append(
            deps,
            &traj,
            &spec.wake,
            "wake/resumed",
            Class::Thought,
            serde_json::json!({ "from_jot": jot.as_str(), "of_wake": spec.wake.as_str() }),
            vec![],
        )
        .await?;
        dispatch(&ctx, &scope).emit::<bough_plugin_agents::AgentContinuation>(
            bough_plugin_agents::Continuation {
                agent: agent.id().clone(),
                wake: spec.wake.clone(),
                from_jot: jot.clone(),
            },
        );
    }

    let mut step_index: u32 = 0;
    // §5 / V10: the attempt count of the CURRENT model step. It survives the retry `continue`,
    // because that is the only way `llm-retry`'s `attempt >= max_attempts` bound is a bound at
    // all; a fresh `attempt: 1` per step made a sustained retryable failure retry forever.
    let mut attempt: u32 = 1;
    let mut last_header: Option<RequestHeader> = None;
    let mut entering: Vec<ClaimedMessage> = claimed;
    let mut concludes;
    let mut reason = WakeEndReason::Completed;
    let mut cause: Option<CancelCause> = None;

    'wake: loop {
        // 4. agent/pre-step: reject, or enter with the messages the model will see.
        let proposed: Vec<LlmMessage> = entering.iter().map(|c| render(&c.message)).collect();
        let pre = dispatch(&ctx, &scope)
            .waterfall::<AgentPreStep>(PreStep {
                agent: agent.id().clone(),
                name: name.clone(),
                wake: spec.wake.clone(),
                kind: spec.kind,
                step_index,
                claimed: entering.clone(),
                decision: PreStepDecision::Enter {
                    messages: proposed.clone(),
                },
            })
            .await;

        let messages = match pre.decision {
            PreStepDecision::Reject { reason: why } => {
                // §5: a rejected or emptied first claim still closes a durable wake that spent no
                // step. The claim stays spliced out: it is the omitter's problem, not the inbox's.
                tracing::debug!(wake = %spec.wake, why, "pre-step rejected the step");
                break 'wake;
            }
            PreStepDecision::Enter { messages } if messages.is_empty() && step_index == 0 => {
                break 'wake;
            }
            PreStepDecision::Enter { messages } => messages,
        };

        // Everything the decision hands over is ledgered before it is shown: a message a listener
        // added is a `mail/delivered` step like any other, so "model-visible ⟺ ledgered" holds
        // for the waterfall's output too, not only for the loop's own input.
        for (i, msg) in messages.iter().enumerate() {
            let origin = entering.get(i).map(|c| &c.message);
            let step = deliver(deps, &traj, &spec.wake, msg, origin).await?;
            // The `mail/delivered` step the loop just wrote IS delivered mail, so this wake
            // consumed it: without that, §5's standing invariant would see it unconsumed forever
            // and schedule drain after drain over the same message.
            consumed.push(SeqRange {
                from: step.seq,
                to: step.seq,
            });
        }
        entering = Vec::new();

        // 5. step/start (durable).
        append(
            deps,
            &traj,
            &spec.wake,
            "step/start",
            Class::Thought,
            serde_json::json!({ "index": step_index }),
            vec![],
        )
        .await?;
        dispatch(&ctx, &scope).emit::<bough_plugin_agents::AgentStep>(
            bough_plugin_agents::StepEvent {
                agent: agent.id().clone(),
                wake: spec.wake.clone(),
                index: step_index,
                phase: bough_plugin_agents::Phase::Start,
            },
        );

        // 6. projection.assemble + the ledger fold. The loop builds every request FROM THE LEDGER
        //    (P2-D19): no in-memory conversation exists to drift from it.
        let as_of = head_seq(deps, &traj).await?;
        let assembled = deps
            .projection
            .0
            .assemble(&AssembleRequest {
                agent: name.clone(),
                wake: Some(spec.wake.clone()),
                at: Utc::now(),
                as_of: Some(as_of),
                budget: None,
            })
            .await
            .map_err(|e| e.to_string())?;
        let of_wake = wake_steps(deps, &traj, &spec.wake).await?;
        let msgs = crate::transcript::rebuild(&of_wake, Some(as_of));

        let facts = Arc::new(RequestFacts {
            agent: name.clone(),
            traj: traj.clone(),
            wake: spec.wake.clone(),
            wake_kind: spec.kind,
            step_index,
            answers_andrey: spec.kind == WakeKind::Answer,
            model_override: model_override(deps, &name).await,
            prompt_ver: deps.cfg.prompt_ver.clone(),
            composition: deps.composition.clone(),
        });
        let budget = assembled.budget;
        let mut inputs = RequestInputs {
            facts: facts.clone(),
            projection: assembled,
            as_of,
            budget,
            tools: deps.tools.schemas(&name),
            call: CallConfig {
                model: String::new(),
                max_tokens: deps.cfg.default_max_tokens,
                effort: None,
                tool_choice_none: false,
                meta: Default::default(),
            },
        };

        // 8. agent/request: a waterfall over the CALL CONFIG ONLY. The loop re-installs its own
        //    facts afterwards (P2-D4), so "a listener cannot mutate messages" is true rather than
        //    documented.
        let decided = dispatch(&ctx, &scope)
            .waterfall::<AgentRequest>(RequestCall {
                facts: facts.clone(),
                call: inputs.call.clone(),
            })
            .await;
        inputs.call = decided.call;
        inputs.facts = facts.clone();

        // 7. request/header, ONLY when it changed (§5). Appended after the call config is decided,
        //    because the call config is one of the four things the header IS.
        if let Some(header) = request::header_if_changed(last_header.as_ref(), &inputs) {
            append(
                deps,
                &traj,
                &spec.wake,
                "request/header",
                Class::Thought,
                request::header_body(&header, &inputs),
                vec![],
            )
            .await?;
            last_header = Some(header);
        }

        // 9. llm/stream. Chunks append as thought steps AS THEY ARE PRODUCED, so a concurrent
        //    answer wake's projection sees everything up to now (§5).
        let req = Arc::new(request::build(&inputs, msgs));
        crate::invariant::record(crate::invariant::SentRequest {
            fiber: ctx.fiber_uid(),
            wake: spec.wake.clone(),
            step_index,
            request: (*req).clone(),
        });
        let outcome = run_step(cell, deps, spec, step_index, req.clone()).await?;

        // 11. step/end (durable).
        append(
            deps,
            &traj,
            &spec.wake,
            "step/end",
            Class::Thought,
            serde_json::to_value(StepEnd {
                index: step_index,
                outcome: if outcome.failure.is_some() {
                    StepOutcome::Error
                } else {
                    StepOutcome::Ok
                },
                detail: outcome.detail.clone(),
            })
            .map_err(|e| e.to_string())?,
            vec![],
        )
        .await?;
        dispatch(&ctx, &scope).emit::<bough_plugin_agents::AgentStep>(
            bough_plugin_agents::StepEvent {
                agent: agent.id().clone(),
                wake: spec.wake.clone(),
                index: step_index,
                phase: bough_plugin_agents::Phase::End,
            },
        );

        // 12. on a failed model step: agent/request-error. A listener that owns recovery returns
        //     `Retry` WITHOUT calling next(); the default leaves the failure terminal.
        if let Some(failure) = outcome.failure.clone() {
            let recovered = dispatch(&ctx, &scope)
                .waterfall::<AgentRequestError>(RequestErrorCall {
                    facts: facts.clone(),
                    request: req.clone(),
                    failure,
                    attempt,
                    recovery: Recovery::Terminal,
                })
                .await;
            match recovered.recovery {
                Recovery::Retry { after } => {
                    tokio::time::sleep(after).await;
                    attempt += 1;
                    step_index += 1;
                    continue 'wake;
                }
                Recovery::Terminal => {
                    reason = WakeEndReason::Error;
                    break 'wake;
                }
            }
        }
        if outcome.max_tokens {
            reason = WakeEndReason::MaxTokens;
            break 'wake;
        }
        // §5: a tool result carrying `concludes_wake` ends the wake AT ITS STEP. It is data, not
        // a listener decision, so it is decided here and the stopping chain still runs and sees it.
        concludes = outcome.concludes;
        if spec.interrupt.is_cancelled() || outcome.interrupted {
            // §5: the answer wake is already running; this one closes as `interrupted` and
            // therefore skips step 16, which IS the mechanism behind "a preempted wake skips its
            // about-line refresh".
            reason = WakeEndReason::Interrupted;
            break 'wake;
        }
        if cancel.is_cancelled() {
            reason = WakeEndReason::Aborted;
            cause = agent.cancelled_by();
            break 'wake;
        }

        // 13. tools owe another request, or next-step input arrived ⇒ another step.
        let next_step_mail = if concludes {
            Vec::new()
        } else {
            claim_next_step(cell, spec).await?
        };
        consumed.extend(crate::mail::consumed_of(&next_step_mail));
        if !concludes && (outcome.owes_another_request || !next_step_mail.is_empty()) {
            entering = next_step_mail;
            step_index += 1;
            attempt = 1;
            continue 'wake;
        }

        // 14. agent/wake-stopping: SERIAL, every listener runs, and the DATA decides — the driver
        //     re-reads the inbox afterwards, so listener order cannot change the outcome (P2-D10).
        dispatch(&ctx, &scope)
            .serial::<AgentWakeStopping>(WakeStopping {
                agent: agent.id().clone(),
                wake: spec.wake.clone(),
                kind: spec.kind,
                steps: step_index + 1,
                concludes,
                handle: agent.clone(),
            })
            .await;
        // The driver RE-READS the inbox after the chain: fresh steering runs another step, none
        // closes the wake. Data decides, so listener ORDER cannot change the outcome (P2-D10).
        let steered = if concludes {
            Vec::new()
        } else {
            claim_next_step(cell, spec).await?
        };
        consumed.extend(crate::mail::consumed_of(&steered));
        if !steered.is_empty() {
            entering = steered;
            step_index += 1;
            attempt = 1;
            continue 'wake;
        }
        if cancel.is_cancelled() {
            reason = WakeEndReason::Aborted;
            cause = agent.cancelled_by();
        }
        break 'wake;
    }

    let out = WakeEndedOutcome {
        reason,
        cause,
        consumed: SeqRange::union(&consumed),
        steps: step_index,
    };
    close_wake(cell, spec, deps, &out, None).await?;
    Ok(out)
}

/// 15–16: `wake/end` (durable), then `agent/wake-end` for COMPLETED wakes only — which is the
/// mechanism behind "a preempted wake skips its about-line refresh" (§5).
async fn close_wake(
    cell: &AgentCell,
    spec: &WakeSpec,
    deps: &LoopDeps,
    out: &WakeEndedOutcome,
    detail: Option<String>,
) -> Result<(), String> {
    let agent = cell.agent().clone();
    let end_step = append(
        deps,
        agent.traj(),
        &spec.wake,
        "wake/end",
        Class::Thought,
        serde_json::to_value(WakeEnd {
            reason: out.reason,
            cause: out
                .cause
                .map(|c| format!("{c:?}").to_lowercase())
                .or(detail),
            consumed: out.consumed.clone(),
        })
        .map_err(|e| e.to_string())?,
        vec![],
    )
    .await?;

    let ctx = agent.ctx().clone();
    let scope = agent.scope_key().clone();
    dispatch(&ctx, &scope).emit::<bough_plugin_agents::AgentWake>(bough_plugin_agents::WakeEvent {
        agent: agent.id().clone(),
        wake: spec.wake.clone(),
        kind: spec.kind,
        phase: bough_plugin_agents::Phase::End,
    });
    if out.reason == WakeEndReason::Completed {
        dispatch(&ctx, &scope)
            .parallel::<AgentWakeEnd>(WakeEndedEvent {
                agent: agent.id().clone(),
                wake: spec.wake.clone(),
                reason: out.reason,
                summary: format!("{} step(s)", out.steps),
                end_step: end_step.id.clone(),
            })
            .await;
    }
    Ok(())
}

/// What one model step produced.
#[derive(Default)]
struct StepOutcomeOf {
    failure: Option<bough_plugin_llm::LlmFailure>,
    detail: Option<String>,
    owes_another_request: bool,
    concludes: bool,
    max_tokens: bool,
    interrupted: bool,
}

/// 9–10: the stream, the tool calls it made, and the results, all durable as they happen.
async fn run_step(
    cell: &AgentCell,
    deps: &LoopDeps,
    spec: &WakeSpec,
    step_index: u32,
    req: Arc<bough_plugin_llm::LlmRequest>,
) -> Result<StepOutcomeOf, String> {
    // The `llm` and `tools` seams register their own innermost hop through the context they are
    // handed and then dispatch from it, so they are handed the ROW's context: a scoped context
    // would make the seam's own hop invisible to its own dispatch.
    let ctx = &deps.ctx;
    let agent = cell.agent().clone();
    let traj = agent.traj().clone();
    let mut out = StepOutcomeOf::default();
    let mut stream = deps.llm.stream(ctx, req, cell.cancel_token()).await;

    let mut text = String::new();
    let mut calls: Vec<ToolCall> = Vec::new();
    let mut last_flush = std::time::Instant::now();

    loop {
        let chunk = tokio::select! {
            biased;
            // An interrupt does not wait for the round to finish: §5's latency promise is that
            // Andrey's answer starts NOW, and this wake stops producing.
            _ = spec.interrupt.cancelled() => {
                out.interrupted = true;
                break;
            }
            next = stream.next() => match next {
                Some(c) => c,
                None => break,
            },
        };
        match chunk {
            Chunk::TextDelta { text: delta } => {
                spec.streamed
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                text.push_str(&delta);
                if last_flush.elapsed().as_millis() as u64 >= deps.cfg.text_flush_ms {
                    flush_text(deps, &traj, &spec.wake, step_index, &mut text).await?;
                    last_flush = std::time::Instant::now();
                }
            }
            Chunk::ReasoningDelta { text: t, meta } => {
                append(
                    deps,
                    &traj,
                    &spec.wake,
                    "thought/reasoning",
                    Class::Thought,
                    serde_json::json!({ "text": t, "meta": meta, "step_index": step_index }),
                    vec![],
                )
                .await?;
            }
            Chunk::ToolCall { id, name, input } => {
                flush_text(deps, &traj, &spec.wake, step_index, &mut text).await?;
                append(
                    deps,
                    &traj,
                    &spec.wake,
                    "tool/call",
                    Class::Thought,
                    serde_json::to_value(ToolCallBody {
                        call: id.clone(),
                        name: name.clone(),
                        args: input.clone(),
                        // §9: the SPEC's declared intent, not a fixed word. This is what a
                        // surface renders the call with — a hardcoded `Generic` here made
                        // `RenderIntent` dead weight and drew every bash call and every diff as
                        // a key/value block.
                        render: deps.tools.render_intent(agent.name(), &name),
                        step_index,
                    })
                    .map_err(|e| e.to_string())?,
                    vec![],
                )
                .await?;
                calls.push(ToolCall {
                    id,
                    name,
                    args: input,
                    agent: agent.name().clone(),
                    wake: spec.wake.clone(),
                    step_index,
                });
            }
            Chunk::Usage(_) => {}
            Chunk::End { stop } => {
                out.max_tokens = stop == StopReason::MaxTokens;
                break;
            }
            Chunk::Failed(f) => {
                out.detail = Some(f.message.clone());
                out.failure = Some(f);
                break;
            }
        }
    }
    flush_text(deps, &traj, &spec.wake, step_index, &mut text).await?;

    if out.interrupted || out.failure.is_some() || calls.is_empty() {
        return Ok(out);
    }

    // 10. the guarded pipeline. Results come back in the MODEL's call order and are appended in
    //     that order, so the durable record is the order the model will read them in (§9).
    // §5: an interrupt or a cancel stops the wake PRODUCING, and that has to reach a tool that is
    // already running. The pipeline runs under a token that fires on either.
    let tool_cancel = tokio_util::sync::CancellationToken::new();
    {
        let t = tool_cancel.clone();
        let interrupt = spec.interrupt.clone();
        let agent_cancel = cell.cancel_token();
        tokio::spawn(async move {
            tokio::select! {
                _ = interrupt.cancelled() => {}
                _ = agent_cancel.cancelled() => {}
                _ = t.cancelled() => return,
            }
            t.cancel();
        });
    }
    let results = deps
        .tools
        .execute_under(ctx, calls, tool_cancel.clone())
        .await;
    // The watcher above is a task, not a leak: cancelling the token ends it.
    tool_cancel.cancel();
    for r in results {
        let outcome = if r.ok {
            ToolOutcomeKind::Ok
        } else {
            match r.failure.as_ref().map(|f| f.kind) {
                Some(bough_plugin_tools::FailureClass::Denied) => ToolOutcomeKind::Denied,
                Some(bough_plugin_tools::FailureClass::Blocked) => ToolOutcomeKind::Blocked,
                Some(bough_plugin_tools::FailureClass::Unknown) => ToolOutcomeKind::Unknown,
                _ => ToolOutcomeKind::Error,
            }
        };
        if r.concludes_wake {
            out.concludes = true;
        }
        let class = if r.cites.is_empty() {
            Class::Thought
        } else {
            Class::Evidence
        };
        append(
            deps,
            &traj,
            &spec.wake,
            "tool/result",
            class,
            serde_json::to_value(ToolResultBody {
                call: r.call.clone(),
                name: r.name.clone(),
                outcome,
                content: r.content.clone(),
                value: r.value.clone(),
                attached: r.attached.clone(),
                concludes_wake: r.concludes_wake,
                step_index,
            })
            .map_err(|e| e.to_string())?,
            r.cites.clone(),
        )
        .await?;
    }
    // A tool answered, so the model owes another request — unless a result concluded the wake.
    out.owes_another_request = !out.concludes;
    Ok(out)
}

/// The one grace step of §5, and P2-D14's promise that a jot ALWAYS exists.
///
/// One model call, tools forbidden, bounded by `grace_deadline_ms`. If it fails, times out or
/// says nothing, the synthetic jot — built deterministically from the wake's last thought steps —
/// is written instead, so a continuation never depends on a model call succeeding.
///
/// It is a REAL step of the interrupted wake: `wake/grace-prompt`, `step/start`,
/// `request/header`, `step/end`, and the request runs the `agent/request` waterfall like any
/// other. It used to build its own `LlmRequest` and call the adapter directly, which meant
/// `model-policy` never assigned it a model (the adapter was handed `""`), nothing durable
/// described it, and V4 could not see it at all — a model-visible input on a side channel.
pub async fn grace_jot(
    cell: &AgentCell,
    deps: &LoopDeps,
    wake: &WakeId,
    kind: WakeKind,
    thoughts: &[Step],
) -> Result<bough_plugin_ledger::StepId, String> {
    let jot = match grace_text(cell, deps, wake, kind, thoughts).await {
        Ok(Some(state)) if !state.trim().is_empty() => WakeJot {
            of_wake: wake.clone(),
            state,
            resume_hint: "resume the interrupted work from the state above".to_string(),
            synthetic: false,
        },
        Ok(_) => crate::preempt::synthetic_jot(wake, thoughts),
        Err(e) => {
            tracing::warn!(%wake, error = %e, "the grace step failed; the synthetic jot stands");
            crate::preempt::synthetic_jot(wake, thoughts)
        }
    };
    write_jot(cell, deps, wake, jot).await
}

/// The grace step's model call. `Ok(None)` on anything that is not plain text in time; `Err` only
/// when the ledger or the projection refuses, which the caller turns into the synthetic jot too.
async fn grace_text(
    cell: &AgentCell,
    deps: &LoopDeps,
    wake: &WakeId,
    kind: WakeKind,
    thoughts: &[Step],
) -> Result<Option<String>, String> {
    let agent = cell.agent().clone();
    let traj = agent.traj().clone();
    let name = agent.name().clone();
    let ctx = agent.ctx().clone();
    let scope = agent.scope_key().clone();

    // The step index of the grace step: one past the wake's last `step/start`.
    let step_index = thoughts
        .iter()
        .filter(|s| s.kind.as_str() == "step/start")
        .filter_map(|s| s.body.get("index").and_then(|v| v.as_u64()))
        .max()
        .map(|i| i as u32 + 1)
        .unwrap_or(0);

    // The instruction, durable BEFORE the request is built: `transcript::rebuild` folds it back
    // into the same user message, so the request reconstructs.
    append(
        deps,
        &traj,
        wake,
        "wake/grace-prompt",
        Class::Thought,
        serde_json::json!({
            "of_wake": wake.to_string(),
            "text": crate::preempt::GRACE_INSTRUCTION,
            "step_index": step_index,
        }),
        vec![],
    )
    .await?;
    let start = append(
        deps,
        &traj,
        wake,
        "step/start",
        Class::Thought,
        serde_json::json!({ "index": step_index }),
        vec![],
    )
    .await?;

    let assembled = deps
        .projection
        .0
        .assemble(&AssembleRequest {
            agent: name.clone(),
            wake: Some(wake.clone()),
            at: Utc::now(),
            as_of: Some(start.seq),
            budget: None,
        })
        .await
        .map_err(|e| e.to_string())?;
    let of_wake = wake_steps(deps, &traj, wake).await?;
    let msgs = crate::transcript::rebuild(&of_wake, Some(start.seq));

    let facts = Arc::new(RequestFacts {
        agent: name.clone(),
        traj: traj.clone(),
        wake: wake.clone(),
        wake_kind: kind,
        step_index,
        // §12: the grace step belongs to the wake it interrupts, so it answers whoever that wake
        // answered. `model-policy` reads exactly this.
        answers_andrey: kind == WakeKind::Answer,
        model_override: model_override(deps, &name).await,
        prompt_ver: deps.cfg.prompt_ver.clone(),
        composition: deps.composition.clone(),
    });
    let budget = assembled.budget;
    let mut inputs = RequestInputs {
        facts: facts.clone(),
        projection: assembled,
        as_of: start.seq,
        budget,
        // §5: the grace step is a JOT, not more work.
        tools: vec![],
        call: CallConfig {
            model: String::new(),
            max_tokens: deps.cfg.default_max_tokens,
            effort: None,
            tool_choice_none: true,
            meta: Default::default(),
        },
    };
    let decided = dispatch(&ctx, &scope)
        .waterfall::<AgentRequest>(RequestCall {
            facts: facts.clone(),
            call: inputs.call.clone(),
        })
        .await;
    inputs.call = decided.call;
    inputs.facts = facts.clone();

    // The grace step's call config differs from the step before it (tools are forbidden), so this
    // header always says something new.
    let header = request::header_of(&inputs);
    append(
        deps,
        &traj,
        wake,
        "request/header",
        Class::Thought,
        request::header_body(&header, &inputs),
        vec![],
    )
    .await?;

    let req = Arc::new(request::build(&inputs, msgs));
    crate::invariant::record(crate::invariant::SentRequest {
        fiber: ctx.fiber_uid(),
        wake: wake.clone(),
        step_index,
        request: (*req).clone(),
    });

    let llm = deps.llm.clone();
    // The stream dispatches on the LOOP's context, exactly as `run_step` does: `LlmHandle::stream`
    // installs its serving hop there, and a hop installed at a different context is invisible to
    // the chain another live round is already holding open.
    let stream_ctx = deps.ctx.clone();
    let cancel = tokio_util::sync::CancellationToken::new();
    let deadline = std::time::Duration::from_millis(deps.cfg.grace_deadline_ms);
    let text = tokio::time::timeout(deadline, async move {
        let mut stream = llm.stream(&stream_ctx, req, cancel).await;
        let mut out = String::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Chunk::TextDelta { text } => out.push_str(&text),
                Chunk::Failed(_) => return None,
                Chunk::End { .. } => break,
                _ => {}
            }
        }
        Some(out)
    })
    .await
    .ok()
    .flatten();

    // What the model said is a thought like any other, so the jot below can cite the wake and the
    // reconstruction of a LATER wake still reads the same bytes.
    if let Some(t) = text.as_ref().filter(|t| !t.trim().is_empty()) {
        flush_text(deps, &traj, wake, step_index, &mut t.clone()).await?;
    }
    append(
        deps,
        &traj,
        wake,
        "step/end",
        Class::Thought,
        serde_json::to_value(StepEnd {
            index: step_index,
            outcome: if text.is_some() {
                StepOutcome::Ok
            } else {
                StepOutcome::Error
            },
            detail: text
                .is_none()
                .then(|| "the grace step produced no text".to_string()),
        })
        .map_err(|e| e.to_string())?,
        vec![],
    )
    .await?;
    Ok(text)
}

/// Append one `wake/jot`.
pub async fn write_jot(
    cell: &AgentCell,
    deps: &LoopDeps,
    wake: &WakeId,
    jot: WakeJot,
) -> Result<bough_plugin_ledger::StepId, String> {
    let traj = cell.agent().traj().clone();
    let step = append(
        deps,
        &traj,
        wake,
        "wake/jot",
        Class::Thought,
        serde_json::to_value(jot).map_err(|e| e.to_string())?,
        vec![],
    )
    .await?;
    Ok(step.id)
}

// ---- small helpers ---------------------------------------------------------------------------

async fn claim_next_step(cell: &AgentCell, spec: &WakeSpec) -> Result<Vec<ClaimedMessage>, String> {
    let mut claimed = cell
        .claim(
            crate::mail::selector_for(spec.kind, Target::NextStep),
            spec.wake.clone(),
            Utc::now(),
        )
        .await
        .map_err(|e| e.to_string())?;
    // The JOIN half of §5's cutoff: mail that arrived before the first streamed token is claimed
    // by THIS wake at its step boundary, off the `next-wake` queue it was addressed to.
    let joining: Vec<MessageId> = std::mem::take(&mut *spec.joined.lock());
    if !joining.is_empty() {
        let mut sel = crate::mail::selector_for(spec.kind, Target::NextWake);
        sel.only = Some(joining);
        claimed.extend(
            cell.claim(sel, spec.wake.clone(), Utc::now())
                .await
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(claimed)
}

/// The model-visible rendering of one message. The SAME shape `transcript::rebuild` folds a
/// `mail/delivered` step into, so a message is identical whether it is read forwards (into the
/// prompt) or backwards (out of the ledger).
pub fn render(msg: &bough_plugin_agents::Message) -> LlmMessage {
    let from = sender_ref(&msg.from);
    let text = if msg.subject.is_empty() {
        format!("[mail from {from}]\n{}", msg.text)
    } else {
        format!("[mail from {from}] {}\n{}", msg.subject, msg.text)
    };
    LlmMessage {
        role: LlmRole::User,
        content: vec![LlmContentBlock::Text { text }],
    }
}

/// The `Ref` spelling of a sender: what `mail/delivered.from` carries and what a routing ref
/// matches against.
pub fn sender_ref(from: &Sender) -> String {
    match from {
        Sender::Andrey => "andrey".to_string(),
        Sender::Agent(name) => format!("agent:{name}"),
        Sender::Worker(id) => format!("worker:{id}"),
        Sender::Collector(c) => format!("collector:{c}"),
        Sender::Ward(w) => format!("ward:{w}"),
        Sender::Hook(h) => format!("hook:{h}"),
        Sender::System(s) => format!("system:{s}"),
    }
}

/// Append the `mail/delivered` step for one entering message.
async fn deliver(
    deps: &LoopDeps,
    traj: &bough_plugin_ledger::TrajId,
    wake: &WakeId,
    msg: &LlmMessage,
    origin: Option<&bough_plugin_agents::Message>,
) -> Result<Step, String> {
    let text: String = msg
        .content
        .iter()
        .filter_map(|b| match b {
            LlmContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let (from, subject, class, mut cites) = match origin {
        Some(o) => (
            sender_ref(&o.from),
            o.subject.clone(),
            o.class,
            o.cites.clone(),
        ),
        // A message the pre-step chain added is from the harness, and it is ledgered exactly like
        // any other: nothing reaches the model through a side channel.
        None => (
            "system:pre-step".to_string(),
            String::new(),
            VMailClass::Ordinary,
            Vec::new(),
        ),
    };
    // The rendering the fold produces is `[mail from x] subject\nsummary`; strip the prefix back
    // off so the round trip is exact.
    let summary = strip_prefix(&text, &from, &subject);
    if cites.is_empty() {
        // `mail/delivered` is EVIDENCE and evidence carries cites (§3): the sender is the source.
        cites.push(Cite {
            r#ref: Ref::new(&from),
            url: None,
        });
    }
    append(
        deps,
        traj,
        wake,
        "mail/delivered",
        Class::Evidence,
        serde_json::to_value(MailDelivered {
            class,
            from: Ref::new(&from),
            subject,
            summary,
            refs: Vec::new(),
        })
        .map_err(|e| e.to_string())?,
        cites,
    )
    .await
}

fn strip_prefix(text: &str, from: &str, subject: &str) -> String {
    let head = if subject.is_empty() {
        format!("[mail from {from}]\n")
    } else {
        format!("[mail from {from}] {subject}\n")
    };
    text.strip_prefix(&head).unwrap_or(text).to_string()
}

async fn flush_text(
    deps: &LoopDeps,
    traj: &bough_plugin_ledger::TrajId,
    wake: &WakeId,
    step_index: u32,
    buf: &mut String,
) -> Result<(), String> {
    if buf.is_empty() {
        return Ok(());
    }
    let text = std::mem::take(buf);
    append(
        deps,
        traj,
        wake,
        "thought/text",
        Class::Thought,
        serde_json::json!({ "text": text, "step_index": step_index }),
        vec![],
    )
    .await?;
    Ok(())
}

async fn append(
    deps: &LoopDeps,
    traj: &bough_plugin_ledger::TrajId,
    wake: &WakeId,
    kind: &str,
    class: Class,
    body: serde_json::Value,
    cites: Vec<Cite>,
) -> Result<Step, String> {
    deps.ledger
        .0
        .append(Append {
            traj: traj.clone(),
            wake: wake.clone(),
            kind: StepType::new(kind),
            class,
            body,
            cites,
            at: Utc::now(),
            id: None,
        })
        .await
        .map_err(|e| e.to_string())
}

async fn head_seq(deps: &LoopDeps, traj: &bough_plugin_ledger::TrajId) -> Result<Seq, String> {
    Ok(deps
        .ledger
        .0
        .head_seq(traj)
        .await
        .map_err(|e| e.to_string())?
        .unwrap_or(Seq(0)))
}

/// The steps one request is built from: the wake's own, plus the mail its `wake/start` claimed.
async fn wake_steps(
    deps: &LoopDeps,
    traj: &bough_plugin_ledger::TrajId,
    wake: &WakeId,
) -> Result<Vec<Step>, String> {
    let all = deps
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    let claimed: Vec<SeqRange> = all
        .iter()
        .filter(|s| &s.wake == wake && s.kind.as_str() == "wake/start")
        .filter_map(|s| s.body.get("claimed").cloned())
        .filter_map(|v| serde_json::from_value::<Vec<SeqRange>>(v).ok())
        .flatten()
        .collect();
    Ok(crate::transcript::steps_for_wake(&all, wake, &claimed))
}

async fn model_override(deps: &LoopDeps, name: &bough_plugin_ledger::AgentName) -> Option<String> {
    deps.ledger
        .0
        .agent(name)
        .await
        .ok()
        .flatten()
        .and_then(|row| row.model_override)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::andrey;

    /// The one thing that makes the fold and the prompt the same object: a message rendered into
    /// a request and the `mail/delivered` step written for it fold back to identical bytes.
    #[test]
    fn a_rendered_message_round_trips_through_the_ledger_shape() {
        let msg = andrey("m1", "ship it");
        let rendered = render(&msg);
        let from = sender_ref(&msg.from);
        let text = match &rendered.content[0] {
            LlmContentBlock::Text { text } => text.clone(),
            other => panic!("{other:?}"),
        };
        let summary = strip_prefix(&text, &from, &msg.subject);
        let step = crate::testing::step(
            1,
            &WakeId::new("w"),
            "mail/delivered",
            serde_json::json!({ "class": "wake", "from": from, "subject": msg.subject,
                                "summary": summary }),
        );
        let folded = crate::transcript::rebuild(&[step], None);
        assert_eq!(folded, vec![rendered]);
    }
}
