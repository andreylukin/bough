//! Invariant: §5's wake flow, with ONLY the model round replaced by a transcript read. Every
//! waterfall runs at its call site and every durable step is appended in §5's order, because a
//! replacement loop is held to the LEDGER PROTOCOL and not to a feature list.
//!
//! Deviation from plan §2.8, which says "steps 6–8 replaced by a transcript read": steps 6–8 are
//! the projection assembly, the `request/header` append and the `agent/request` waterfall, and
//! dropping them would make this row's own invariant (the request reconstructs from the ledger)
//! vacuous and would delete the `request/header` its test asserts on. What the transcript
//! replaces is step 9, the model round. The plan document is the bug; this comment is the fix.
//!
//! Everything here takes its clock, its ledger and its recorder as arguments: a replay is a pure
//! function of (script, ledger, inputs) and a test needs no scheduler.

use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_agents::{
    AgentPreStep, AgentWakeEnd, AgentWakeStopping, PreStep, PreStepDecision, WakeEnded,
    WakeStopping,
};
use bough_plugin_ledger::vocabulary::{
    SpliceOp, SpliceTarget, StepOutcome, Urgency, WakeEndReason,
};
use bough_plugin_ledger::{
    Append, Class, LedgerHandle, Order, Seq, SeqRange, Step, StepId, StepQuery, StepType, TrajId,
    WakeId,
};
use bough_plugin_llm::{
    AgentRequest, CallConfig, Chunk, LlmRequest, RequestCall, RequestFacts, WakeKind,
};
use bough_plugin_projection::{AssembleRequest, ProjectionHandle};
use chrono::{DateTime, Utc};

use crate::script::Script;

/// What the replay could not do. A scripted run fails LOUD (§0.2): running out of script under
/// `strict` is an error, never a silent idle.
#[derive(Debug, thiserror::Error)]
pub enum ReplayError {
    #[error("the script has no wake at index {0}")]
    NoSuchWake(usize),
    #[error(transparent)]
    Ledger(#[from] bough_plugin_ledger::LedgerError),
    #[error("serialising a step body failed: {0}")]
    Body(#[from] serde_json::Error),
    #[error("the script issued a tool call but no tools handle is mounted")]
    NoTools,
}

/// One request as the invariant's recorder wants it, without depending on `agent-loop`'s type at
/// this call site: `apply` adapts it (P2-D18 keeps the EVALUATOR shared, not the plumbing).
#[derive(Clone, Debug)]
pub struct Recorded {
    pub wake: WakeId,
    pub step_index: u32,
    pub request: LlmRequest,
}

/// The recorder the loop hands each request it "sent". `None` in a test.
pub type Recorder = Arc<dyn Fn(Recorded) + Send + Sync>;

/// Everything a replay needs that is not the script.
#[derive(Clone)]
pub struct ReplayEnv {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    /// `None` ⇒ no projection is mounted and the request carries no sections. The scripted row
    /// injects `projection`, so this is `Some` in every composed tree.
    pub projection: Option<ProjectionHandle>,
    pub script: Arc<Script>,
    pub strict: bool,
    pub prompt_ver: String,
    pub composition: String,
    pub default_max_tokens: i64,
    pub recorder: Option<Recorder>,
    /// The tools seam. `None` in a unit test that scripts no tool call; `Some` in every composed
    /// tree, because the row injects `tools`. A scripted `tool/call` is DISPATCHED through the
    /// same guarded pipeline `agent-loop` uses — appending the call and walking on left a
    /// dangling call in the ledger and made "tools keep working unchanged" an activation check
    /// rather than a functional one.
    pub tools: Option<bough_plugin_tools::ToolsHandle>,
}

/// One inbox item this wake claims, as the transcript's driver knows it.
#[derive(Clone, Debug)]
pub struct ScriptedClaim {
    pub message: String,
    pub target: SpliceTarget,
    pub wake: bool,
    /// The seq of the `mail/delivered` step, if this is delivered mail. Consumption is per
    /// (agent, seq) and applies to DELIVERED mail only (§5).
    pub mail_seq: Option<Seq>,
}

/// One piece of mail this wake shows the model, appended as a `mail/delivered` step.
///
/// §5 makes consumption per (agent, seq) over DELIVERED mail, so a replayed wake that claims
/// mail must ledger the delivery too — otherwise its `wake/end.consumed` set is empty however
/// much mail it took, and the model never sees the message it woke for.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliveredMail {
    pub summary: String,
    pub from: String,
    pub class: String,
    pub subject: String,
}

/// One wake of the transcript.
#[derive(Clone, Debug)]
pub struct WakeInput {
    pub traj: TrajId,
    pub agent: bough_plugin_ledger::AgentName,
    pub agent_id: bough_plugin_agents::AgentId,
    pub wake: WakeId,
    /// Which scripted wake to replay.
    pub index: usize,
    pub kind: WakeKind,
    pub urgency: Urgency,
    pub trigger: Option<StepId>,
    pub answers_andrey: bool,
    pub model_override: Option<String>,
    pub claim: Vec<ScriptedClaim>,
    /// The mail this wake delivers to the model (the driver's claimed messages).
    pub deliver: Vec<DeliveredMail>,
    /// The live handle `agent/wake-stopping` carries, so a listener that wants to bound a
    /// runaway wake can cancel (§5). `None` in a replay driven without the `agents` seam — a
    /// test of the durable protocol — and `Some` in every composed tree, where the driver has
    /// the cell.
    pub handle: Option<bough_plugin_agents::Agent>,
    pub at: DateTime<Utc>,
}

/// What one replayed wake produced.
#[derive(Clone, Debug, PartialEq)]
pub struct WakeOutcome {
    pub wake: WakeId,
    pub reason: WakeEndReason,
    pub steps: u32,
    pub consumed: Vec<SeqRange>,
    pub end_step: StepId,
}

async fn append(
    env: &ReplayEnv,
    input: &WakeInput,
    kind: &str,
    class: Class,
    body: serde_json::Value,
) -> Result<Step, ReplayError> {
    append_citing(env, input, kind, class, body, vec![]).await
}

/// The same append, carrying cites: evidence requires them (§3).
async fn append_citing(
    env: &ReplayEnv,
    input: &WakeInput,
    kind: &str,
    class: Class,
    body: serde_json::Value,
    cites: Vec<bough_plugin_ledger::Cite>,
) -> Result<Step, ReplayError> {
    Ok(env
        .ledger
        .0
        .append(Append {
            traj: input.traj.clone(),
            wake: input.wake.clone(),
            kind: StepType::new(kind),
            class,
            body,
            cites,
            at: input.at,
            id: None,
        })
        .await?)
}

/// Replay one wake, appending §5's durable steps in §5's order.
pub async fn run_wake(env: &ReplayEnv, input: &WakeInput) -> Result<WakeOutcome, ReplayError> {
    let n_steps = match env.script.steps_in(input.index) {
        Some(n) => n,
        None if env.strict => return Err(ReplayError::NoSuchWake(input.index)),
        None => 0,
    };

    // §5 step 2 — `wake/start`, carrying the ranges this wake claims before running.
    let consumed: Vec<SeqRange> = SeqRange::union(
        &input
            .claim
            .iter()
            .filter_map(|c| c.mail_seq)
            .map(|s| SeqRange { from: s, to: s })
            .collect::<Vec<_>>(),
    );
    append(
        env,
        input,
        "wake/start",
        Class::Thought,
        serde_json::json!({
            "urgency": input.urgency,
            "trigger": input.trigger,
            "claimed": consumed,
        }),
    )
    .await?;

    // §2's `agent/wake` START moment, emitted by BOTH loop Providers.
    env.ctx
        .emit::<bough_plugin_agents::AgentWake>(bough_plugin_agents::WakeEvent {
            agent: input.agent_id.clone(),
            wake: input.wake.clone(),
            kind: input.kind,
            phase: bough_plugin_agents::Phase::Start,
        });

    // §5 step 3 — the claim is a pure DELETION splice: one `inbox/spliced { op: claim }` per
    // message, durable before any of it reaches the model.
    let mut claim_steps: Vec<StepId> = Vec::new();
    for c in &input.claim {
        let s = append(
            env,
            input,
            "inbox/spliced",
            Class::Thought,
            serde_json::json!({
                "message": c.message,
                "op": SpliceOp::Claim,
                "target": c.target,
                "wake": c.wake,
            }),
        )
        .await?;
        claim_steps.push(s.id);
    }

    // §5 step 4a — the claimed mail becomes durable, model-visible `mail/delivered` steps, and
    // their seqs are what this wake CONSUMED (§5: consumption is per (agent, seq) over delivered
    // mail).
    let mut consumed = consumed;
    for m in &input.deliver {
        // `mail/delivered` is EVIDENCE and evidence carries cites (§3): the sender is the source.
        let step = env
            .ledger
            .0
            .append(Append {
                traj: input.traj.clone(),
                wake: input.wake.clone(),
                kind: StepType::new("mail/delivered"),
                class: Class::Evidence,
                body: serde_json::json!({
                    "class": m.class,
                    "from": m.from,
                    "refs": Vec::<String>::new(),
                    "subject": m.subject,
                    "summary": m.summary,
                }),
                cites: vec![bough_plugin_ledger::Cite {
                    r#ref: bough_plugin_ledger::Ref::new(&m.from),
                    url: None,
                }],
                at: input.at,
                id: None,
            })
            .await?;
        consumed.push(SeqRange {
            from: step.seq,
            to: step.seq,
        });
    }
    let consumed = SeqRange::union(&consumed);

    // §5 step 4 — `agent/pre-step`. A Reject still closes a durable wake that spent no step.
    let pre = env
        .ctx
        .waterfall::<AgentPreStep>(PreStep {
            agent: input.agent_id.clone(),
            name: input.agent.clone(),
            wake: input.wake.clone(),
            kind: input.kind,
            step_index: 0,
            claimed: Vec::new(),
            decision: PreStepDecision::Enter {
                messages: Vec::new(),
            },
        })
        .await;
    if let PreStepDecision::Reject { reason } = &pre.decision {
        return end_wake(
            env,
            input,
            WakeEndReason::Completed,
            Some(reason.clone()),
            0,
            consumed,
        )
        .await;
    }

    let mut last_header: Option<serde_json::Value> = None;
    let mut reason = WakeEndReason::Completed;
    let mut ran: u32 = 0;

    for index in 0..n_steps {
        let idx = index as u32;
        // §5 step 5 — `step/start`.
        append(
            env,
            input,
            "step/start",
            Class::Thought,
            serde_json::json!({ "index": idx }),
        )
        .await?;
        ran += 1;

        // §5 step 6 — the projection, and the messages rebuilt FROM THE LEDGER.
        let as_of = env.ledger.0.head_seq(&input.traj).await?;
        let (sections, projection_text) = assemble(env, input).await;
        let messages = rebuild(&steps_of_wake(env, input).await?);

        // §5 step 8 — `agent/request`, a waterfall over the CALL CONFIG ONLY. The facts are
        // behind an `Arc` and this loop re-installs its own copy afterwards.
        let facts = Arc::new(RequestFacts {
            agent: input.agent.clone(),
            traj: input.traj.clone(),
            wake: input.wake.clone(),
            wake_kind: input.kind,
            step_index: idx,
            answers_andrey: input.answers_andrey,
            model_override: input.model_override.clone(),
            prompt_ver: env.prompt_ver.clone(),
            composition: env.composition.clone(),
        });
        let decided = env
            .ctx
            .waterfall::<AgentRequest>(RequestCall {
                facts: facts.clone(),
                call: CallConfig {
                    model: String::new(),
                    max_tokens: env.default_max_tokens,
                    effort: None,
                    tool_choice_none: false,
                    meta: Default::default(),
                },
            })
            .await;

        let request = LlmRequest {
            model: decided.call.model.clone(),
            system: Some(projection_text.clone()),
            system_volatile: None,
            messages,
            tools: Vec::new(),
            call: decided.call.clone(),
        };

        // §5 step 7 — `request/header`, ONLY when it differs from the last one in this wake.
        let header = serde_json::json!({
            "prompt_ver": env.prompt_ver,
            "sections": sections,
            "tools": Vec::<String>::new(),
            "call": serde_json::json!({
                "model": decided.call.model,
                "max_tokens": decided.call.max_tokens,
                "tool_choice_none": decided.call.tool_choice_none,
            }),
            "composition": env.composition,
            "as_of": as_of,
            "budget": 0,
            "projection_digest": digest(&projection_text),
        });
        // "ONLY when it differs from the last one in this wake" (§5) is a statement about what
        // the MODEL was shown: `as_of` is a ledger position that moves with every append, so
        // comparing it would append a header per step and say nothing. Everything else — the
        // sections, the tools, the call config and the projection digest — is compared.
        let shown = {
            let mut h = header.clone();
            h.as_object_mut().expect("an object").remove("as_of");
            h
        };
        if last_header.as_ref() != Some(&shown) {
            append(env, input, "request/header", Class::Thought, header).await?;
            last_header = Some(shown);
        }

        if let Some(rec) = &env.recorder {
            rec(Recorded {
                wake: input.wake.clone(),
                step_index: idx,
                request: request.clone(),
            });
        }

        // §5 step 9 — the transcript stands in for the model round. Every chunk appends AS IT IS
        // PRODUCED, so an interrupted replay leaves exactly what a live interruption would.
        let chunks = env.script.chunks(input.index, index).unwrap_or_default();
        let mut outcome = StepOutcome::Ok;
        let mut detail: Option<String> = None;
        let mut calls: Vec<bough_plugin_tools::ToolCall> = Vec::new();
        for chunk in chunks {
            match chunk {
                Chunk::TextDelta { text } => {
                    append(
                        env,
                        input,
                        "thought/text",
                        Class::Thought,
                        serde_json::json!({ "text": text, "step_index": idx }),
                    )
                    .await?;
                }
                Chunk::ReasoningDelta { text, meta } => {
                    append(
                        env,
                        input,
                        "thought/reasoning",
                        Class::Thought,
                        serde_json::json!({ "text": text, "meta": meta, "step_index": idx }),
                    )
                    .await?;
                }
                Chunk::ToolCall {
                    id,
                    name,
                    input: args,
                } => {
                    append(
                        env,
                        input,
                        "tool/call",
                        Class::Thought,
                        serde_json::json!({
                            "call": id,
                            "name": name,
                            "args": args,
                            "render": "generic",
                            "step_index": idx,
                        }),
                    )
                    .await?;
                    calls.push(bough_plugin_tools::ToolCall {
                        id,
                        name,
                        args,
                        agent: input.agent.clone(),
                        wake: input.wake.clone(),
                        step_index: idx,
                    });
                }
                Chunk::Usage(_) => {}
                Chunk::End { stop } => {
                    if stop == bough_plugin_llm::StopReason::MaxTokens {
                        reason = WakeEndReason::MaxTokens;
                    }
                }
                Chunk::Failed(f) => {
                    // §12: a failure is a terminal chunk, never an error. The wake ends with
                    // reason `error`; this row implements no retry (that is `agent-loop`'s).
                    outcome = StepOutcome::Error;
                    detail = Some(f.message.clone());
                    reason = WakeEndReason::Error;
                }
            }
        }

        // §5 step 10 — the guarded pipeline, the SAME one `agent-loop` runs, and the results are
        // appended in the model's call order (§9).
        if !calls.is_empty() {
            let Some(tools) = env.tools.clone() else {
                return Err(ReplayError::NoTools);
            };
            for r in tools.execute(&env.ctx, calls).await {
                let class = if r.cites.is_empty() {
                    Class::Thought
                } else {
                    Class::Evidence
                };
                let outcome_kind = if r.ok {
                    "ok"
                } else {
                    match r.failure.as_ref().map(|f| f.kind) {
                        Some(bough_plugin_tools::FailureClass::Denied) => "denied",
                        Some(bough_plugin_tools::FailureClass::Blocked) => "blocked",
                        Some(bough_plugin_tools::FailureClass::Unknown) => "unknown",
                        _ => "error",
                    }
                };
                append_citing(
                    env,
                    input,
                    "tool/result",
                    class,
                    serde_json::json!({
                        "call": r.call,
                        "name": r.name,
                        "outcome": outcome_kind,
                        "content": r.content,
                        "value": r.value,
                        "attached": r.attached,
                        "concludes_wake": r.concludes_wake,
                        "step_index": idx,
                    }),
                    r.cites.clone(),
                )
                .await?;
            }
        }

        // §5 step 11 — `step/end`.
        append(
            env,
            input,
            "step/end",
            Class::Thought,
            serde_json::json!({ "index": idx, "outcome": outcome, "detail": detail }),
        )
        .await?;

        if reason != WakeEndReason::Completed {
            break;
        }
    }

    // §5 step 14 — `agent/wake-stopping`, SERIAL: every listener runs, in order.
    if let Some(handle) = &input.handle {
        env.ctx
            .serial::<AgentWakeStopping>(WakeStopping {
                agent: input.agent_id.clone(),
                wake: input.wake.clone(),
                kind: input.kind,
                steps: ran,
                concludes: false,
                handle: handle.clone(),
            })
            .await;
    }

    end_wake(env, input, reason, None, ran, consumed).await
}

/// §5 steps 15–16: the durable close, and the wake-end moment for COMPLETED wakes only.
async fn end_wake(
    env: &ReplayEnv,
    input: &WakeInput,
    reason: WakeEndReason,
    cause: Option<String>,
    steps: u32,
    consumed: Vec<SeqRange>,
) -> Result<WakeOutcome, ReplayError> {
    let end = append(
        env,
        input,
        "wake/end",
        Class::Thought,
        serde_json::json!({ "reason": reason, "cause": cause, "consumed": consumed }),
    )
    .await?;

    env.ctx
        .emit::<bough_plugin_agents::AgentWake>(bough_plugin_agents::WakeEvent {
            agent: input.agent_id.clone(),
            wake: input.wake.clone(),
            kind: input.kind,
            phase: bough_plugin_agents::Phase::End,
        });

    if reason == WakeEndReason::Completed {
        // COMPLETED only: an interrupted wake refreshes no about-line (§5).
        env.ctx
            .parallel::<AgentWakeEnd>(WakeEnded {
                agent: input.agent_id.clone(),
                wake: input.wake.clone(),
                reason,
                summary: String::new(),
                end_step: end.id.clone(),
            })
            .await;
    }

    Ok(WakeOutcome {
        wake: input.wake.clone(),
        reason,
        steps,
        consumed,
        end_step: end.id,
    })
}

async fn steps_of_wake(env: &ReplayEnv, input: &WakeInput) -> Result<Vec<Step>, ReplayError> {
    Ok(env
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![input.traj.clone()],
            wake: Some(input.wake.clone()),
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?)
}

/// The messages the model is shown, folded from the wake's own steps.
///
/// The live loop imports `agent_loop::transcript::rebuild` for this; this row keeps its own fold
/// so a broken `agent-loop` cannot make the swap gate pass by accident. The two are checked
/// against each other by the SHARED reconstruction evaluator (P2-D18), which is the thing that
/// must not drift.
fn rebuild(steps: &[Step]) -> Vec<bough_plugin_llm::LlmMessage> {
    use bough_plugin_llm::{LlmContentBlock, LlmRole};
    let mut out: Vec<bough_plugin_llm::LlmMessage> = Vec::new();
    for s in steps {
        let (role, text) = match s.kind.as_str() {
            // The canonical envelope (§5) is `[mail from {from}] {subject}\n{summary}`: the model
            // is shown who wrote it, not a bare line. Rendering only the summary here made the
            // shared reconstruction evaluator fail — which is exactly its job.
            "mail/delivered" => {
                let field = |k: &str| {
                    s.body
                        .get(k)
                        .and_then(|v| v.as_str())
                        .unwrap_or_default()
                        .to_string()
                };
                let (from, subject, summary) = (field("from"), field("subject"), field("summary"));
                let head = if subject.is_empty() {
                    format!("[mail from {from}]")
                } else {
                    format!("[mail from {from}] {subject}")
                };
                (LlmRole::User, format!("{head}\n{summary}"))
            }
            "thought/text" => (
                LlmRole::Assistant,
                s.body
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            ),
            _ => continue,
        };
        if text.is_empty() {
            continue;
        }
        out.push(bough_plugin_llm::LlmMessage {
            role,
            content: vec![LlmContentBlock::Text { text }],
        });
    }
    out
}

async fn assemble(env: &ReplayEnv, input: &WakeInput) -> (Vec<String>, String) {
    let Some(p) = &env.projection else {
        return (Vec::new(), String::new());
    };
    match p
        .0
        .assemble(&AssembleRequest {
            agent: input.agent.clone(),
            wake: Some(input.wake.clone()),
            at: input.at,
            budget: None,
            as_of: None,
        })
        .await
    {
        Ok(a) => (
            a.sections.iter().map(|s| s.id.to_string()).collect(),
            a.to_text(),
        ),
        // An answer wake must always be buildable (§5): a projection that cannot assemble
        // degrades to an empty context rather than losing the wake.
        Err(_) => (Vec::new(), String::new()),
    }
}

fn digest(text: &str) -> String {
    use sha2::Digest;
    let mut h = sha2::Sha256::new();
    h.update(text.as_bytes());
    format!("{:x}", h.finalize())
}

/// The durable SHAPE of a trajectory: kind, class, body and cite count per step, in seq order.
///
/// Ids and clocks are excluded on purpose — they are the two things a replay cannot make equal
/// across two runs, and they are also the two things the ledger protocol does not fix. What is
/// left is exactly the claim "the same transcript replays the same way".
pub fn dump(steps: &[Step]) -> String {
    let mut out = String::new();
    for s in steps {
        out.push_str(s.kind.as_str());
        out.push(' ');
        out.push_str(s.class.as_str());
        out.push(' ');
        out.push_str(&serde_json::to_string(&*s.body).unwrap_or_default());
        out.push_str(&format!(" cites={}\n", s.cites.len()));
    }
    out
}
