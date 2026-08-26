//! Invariant: one pass, three appends and one seam call — and nothing else. Distillation goes
//! through `ctx.rollups.rebuild_digest(DigestRequest { from_raw: false, .. })`, so "reconsolidation
//! adds a block" and "the summarizer seals a block" are ONE code path and cannot disagree about
//! `prompt_ver`, `sealed_at` or the `rollup/sealed` step (P4-D6).
//!
//! Nothing in this module calls `seal_rollup`, `supersede_rollup`, or any write on a step: the
//! only writes are `ledger.append` of this crate's own kinds and the one seam call.

use std::sync::Arc;

use bough_plugin_ledger::{
    Append, Cite, Class, HashScope, Order, Ref, Step, StepQuery, StepType, WakeId,
};
use bough_plugin_llm::{
    CallConfig, Chunk, LlmContentBlock, LlmMessage, LlmRequest, LlmRole, RequestCall, RequestFacts,
    WakeKind,
};
use bough_plugin_rollups::DigestRequest;
use futures::StreamExt;

use crate::vocabulary::MemoryExpired;
use crate::{
    detect, Candidate, PassPlan, PassReport, PassRequest, ReconError, ReconInner, ReconKind,
    ReconPassId, StaleReason, MEMORY_EXPIRED,
};

/// The synthetic wake every step of a pass carries (P4-D2): a pass is not a model turn, but a
/// step still needs a wake id and inventing a fresh one per append would scatter one act across
/// many wakes.
pub fn pass_wake(pass: &ReconPassId) -> WakeId {
    WakeId::new(format!("recon:{pass}"))
}

/// The word the judge must say for a pair to become a claim. Anything else — including silence,
/// a refusal or a failed call — CLEARS the pair: a contradiction nobody asserted is not one.
pub const CONFIRMED: &str = "CONTRADICTION";

/// PURE: whether a judge's answer confirms the pair.
pub fn confirms(answer: &str) -> bool {
    answer
        .lines()
        .any(|l| l.trim_start().to_ascii_uppercase().starts_with(CONFIRMED))
}

/// The steps a pass reads, and the range they cover.
async fn batch(
    inner: &ReconInner,
    req: &PassRequest,
) -> Result<(Vec<Step>, Option<u64>), ReconError> {
    let head = inner.ledger.0.head_seq(&req.traj).await?;
    let Some(head) = head else {
        return Ok((Vec::new(), None));
    };
    let from = match req.since {
        Some(s) => s.0.max(1),
        None => head
            .0
            .saturating_sub(inner.cfg.batch_steps as u64 - 1)
            .max(1),
    };
    let steps = inner
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![req.traj.clone()],
            after: from.checked_sub(1).map(bough_plugin_ledger::Seq),
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?;
    Ok((steps, Some(from)))
}

/// What a pass WOULD do. No model call, no write.
pub async fn plan(inner: &ReconInner, req: &PassRequest) -> Result<PassPlan, ReconError> {
    let (steps, from) = batch(inner, req).await?;
    let range = bough_plugin_ledger::SeqRange {
        from: bough_plugin_ledger::Seq(from.unwrap_or(0)),
        to: steps
            .last()
            .map(|s| s.seq)
            .unwrap_or(bough_plugin_ledger::Seq(from.unwrap_or(0))),
    };
    Ok(PassPlan {
        range,
        distil: !steps.is_empty(),
        contradiction_candidates: detect::pairs(&steps, inner.cfg.max_contradiction_pairs),
        expiry_candidates: detect::stale(&steps, req.at, &inner.cfg),
    })
}

/// Run the pass.
pub async fn run(inner: &ReconInner, req: &PassRequest) -> Result<PassReport, ReconError> {
    let pass = ReconPassId::new(uuid::Uuid::now_v7().to_string());
    let wake = pass_wake(&pass);
    let plan = plan(inner, req).await?;

    // Everything the ledger holds BEFORE the pass writes anything. The invariant re-reads these
    // at quiesce, so "a pass adds and never edits" is checked against what was actually there.
    let before = inner.ledger.0.row_hashes(HashScope::All).await?;

    let budget = req.max_calls.unwrap_or(inner.cfg.max_calls_per_pass);
    let mut calls = 0usize;
    let mut tokens_in = 0u64;
    let mut tokens_out = 0u64;
    let mut appended: Vec<(bough_plugin_ledger::StepId, StepType)> = Vec::new();
    let mut contradictions = Vec::new();
    // Steps a confirmed contradiction made stale, and the claim that says so.
    let mut contradicted: Vec<(bough_plugin_ledger::StepId, bough_plugin_ledger::StepId)> =
        Vec::new();

    for pair in &plan.contradiction_candidates {
        if calls >= budget {
            break;
        }
        let (answer, used) = judge(inner, req, pair).await;
        calls += 1;
        tokens_in += used.0;
        tokens_out += used.1;
        if !confirms(&answer) {
            continue;
        }
        let (claim, cites) = detect::contradiction_claim(pair, &answer);
        let step = inner
            .ledger
            .0
            .append(Append {
                traj: req.traj.clone(),
                wake: wake.clone(),
                kind: StepType::new("claim/proposed"),
                class: Class::Thought,
                body: serde_json::to_value(&claim).expect("ClaimProposed serialises"),
                cites,
                at: req.at,
                id: None,
            })
            .await?;
        appended.push((step.id.clone(), step.kind.clone()));
        // The OLDER half is what a confirmed contradiction makes stale; the newer one stands.
        contradicted.push((pair.older.clone(), step.id.clone()));
        contradictions.push(step.id);
    }

    // Stale evidence: one APPENDED marker per candidate, citing what justified it.
    let mut expired = Vec::new();
    let mut candidates: Vec<(Candidate, Option<bough_plugin_ledger::StepId>)> = plan
        .expiry_candidates
        .iter()
        .cloned()
        .map(|c| (c, None))
        .collect();
    for (step, claim) in contradicted {
        if candidates.iter().any(|(c, _)| c.step == step) {
            continue;
        }
        candidates.push((
            Candidate {
                step,
                kind: StepType::new("claim/proposed"),
                age_days: 0,
                why: StaleReason::Contradicted,
            },
            Some(claim),
        ));
    }
    for (candidate, claim) in candidates {
        let target = Ref::new(format!("step:{}", candidate.step));
        let (reason, mut cites) = match (&candidate.why, &claim) {
            (StaleReason::Age, _) => (
                format!(
                    "no longer load-bearing: {} days old, past the {}-day threshold",
                    candidate.age_days, inner.cfg.stale_after_days
                ),
                vec![Cite {
                    r#ref: target.clone(),
                    url: None,
                }],
            ),
            (StaleReason::Contradicted, Some(c)) => (
                format!("contradicted by later evidence; see the claim in `step:{c}`"),
                vec![
                    Cite {
                        r#ref: target.clone(),
                        url: None,
                    },
                    Cite {
                        r#ref: Ref::new(format!("step:{c}")),
                        url: None,
                    },
                ],
            ),
            (StaleReason::Contradicted, None) => (
                "contradicted by later evidence".to_string(),
                vec![Cite {
                    r#ref: target.clone(),
                    url: None,
                }],
            ),
        };
        cites.dedup();
        let step = inner
            .ledger
            .0
            .append(Append {
                traj: req.traj.clone(),
                wake: wake.clone(),
                kind: StepType::new(MEMORY_EXPIRED),
                // EVIDENCE, so the ledger itself refuses a marker that cannot say what justified
                // it (§3's two-entry-class rule).
                class: Class::Evidence,
                body: serde_json::to_value(MemoryExpired {
                    targets: vec![target],
                    reason,
                    kind: ReconKind::Expiry,
                })
                .expect("MemoryExpired serialises"),
                cites,
                at: req.at,
                id: None,
            })
            .await?;
        appended.push((step.id.clone(), step.kind.clone()));
        expired.push(step.id);
    }

    // The distilled block: ADDED through the rollups seam, never sealed here.
    let distilled = if plan.distil {
        let report = inner
            .rollups
            .0
            .rebuild_digest(&DigestRequest {
                agent: req.agent.clone(),
                traj: req.traj.clone(),
                at: req.at,
                attribution: req.attribution.clone(),
                // NOT a reset: a pass distils on top of what already stands (§8). `/reset` is
                // drift-watch's, and it is the only caller that passes `true`.
                from_raw: false,
            })
            .await?;
        calls += report.calls;
        Some(report.digest)
    } else {
        None
    };

    crate::invariant::record(crate::invariant::Obs {
        pass: pass.clone(),
        appended,
        before,
    });

    Ok(PassReport {
        pass,
        distilled,
        contradictions,
        expired,
        calls,
        tokens_in,
        tokens_out,
    })
}

/// Ask the model whether a pair really conflicts. Returns the verdict text and `(in, out)` tokens.
///
/// A failed call CLEARS the pair: an unreachable model must never manufacture a claim.
async fn judge(inner: &ReconInner, req: &PassRequest, pair: &crate::Pair) -> (String, (u64, u64)) {
    let older = inner.ledger.0.step(&pair.older).await.ok().flatten();
    let newer = inner.ledger.0.step(&pair.newer).await.ok().flatten();
    let (Some(older), Some(newer)) = (older, newer) else {
        return (String::new(), (0, 0));
    };

    let facts = Arc::new(RequestFacts {
        agent: req.agent.clone(),
        traj: req.traj.clone(),
        wake: pass_wake(&ReconPassId::new("plan")),
        // A governance pass is unattended work: `model-policy` reads exactly this and picks terra.
        wake_kind: WakeKind::Scheduled,
        step_index: 0,
        answers_andrey: false,
        model_override: None,
        prompt_ver: "recon-1".to_string(),
        composition: String::new(),
    });
    let call = CallConfig {
        model: String::new(),
        max_tokens: inner.cfg.distill_max_tokens,
        effort: None,
        tool_choice_none: true,
        meta: Default::default(),
    };
    let decided = inner
        .ctx
        .waterfall::<bough_plugin_llm::AgentRequest>(RequestCall {
            facts,
            call: call.clone(),
        })
        .await;

    let request = Arc::new(LlmRequest {
        model: decided.call.model.clone(),
        system: Some(SYSTEM.to_string()),
        system_volatile: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text {
                text: prompt(&older, &newer, &pair.shared),
            }],
        }],
        tools: vec![],
        call: decided.call,
    });

    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = inner.llm.stream(&inner.ctx, request, cancel).await;
    let mut text = String::new();
    let mut used = (0u64, 0u64);
    while let Some(chunk) = stream.next().await {
        match chunk {
            Chunk::TextDelta { text: t } => text.push_str(&t),
            Chunk::Usage(u) => used = (u.input_tokens.max(0) as u64, u.output_tokens.max(0) as u64),
            // A failure is not a verdict: the pair clears.
            Chunk::Failed(_) => return (String::new(), used),
            Chunk::End { .. } => break,
            _ => {}
        }
    }
    (text, used)
}

/// The judge's standing instruction. A pass may only ever SURFACE a disagreement as a proposal;
/// §8 makes the accept/reject surface Phase 5's, so the prompt never asks for a resolution.
pub const SYSTEM: &str = "\
You are checking two pieces of recorded evidence for a factual contradiction. \
Answer with a single line starting with the word CONTRADICTION followed by one sentence naming \
what disagrees, or the single word CLEAR if they are merely different, complementary, or about \
different things. Do not resolve the disagreement and do not speculate beyond what is written.";

/// PURE: what the judge is shown. A unit of the prompt, so a bad rendering is a test failure.
pub fn prompt(older: &Step, newer: &Step, shared: &[Ref]) -> String {
    let render = |s: &Step| {
        format!(
            "kind: {}\nat: {}\nbody: {}",
            s.kind,
            s.at.to_rfc3339(),
            serde_json::to_string(&*s.body).unwrap_or_default()
        )
    };
    format!(
        "shared refs: {}\n\n--- earlier (step:{}) ---\n{}\n\n--- later (step:{}) ---\n{}\n",
        shared
            .iter()
            .map(|r| r.to_string())
            .collect::<Vec<_>>()
            .join(", "),
        older.id,
        render(older),
        newer.id,
        render(newer),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_confirming_word_confirms() {
        assert!(confirms("CONTRADICTION: the port differs"));
        assert!(confirms("  contradiction: the port differs"));
        assert!(confirms("thinking...\nCONTRADICTION: yes"));
        assert!(!confirms("CLEAR"));
        assert!(!confirms(""));
        assert!(
            !confirms("there is no contradiction here"),
            "the word must OPEN a line, or every clearing answer would confirm"
        );
    }

    #[test]
    fn the_pass_wake_names_the_pass() {
        let p = ReconPassId::new("abc");
        assert_eq!(pass_wake(&p).as_str(), "recon:abc");
    }
}
