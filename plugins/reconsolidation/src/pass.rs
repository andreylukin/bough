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

use crate::vocabulary::{MemoryExpired, ReconRequest};
use crate::{
    detect, Candidate, PassPlan, PassReport, PassRequest, ReconError, ReconInner, ReconKind,
    ReconPassId, StaleReason, MEMORY_EXPIRED, RECON_REQUEST,
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

/// The call budget for one pass. An explicit `resolve(request) -> Spec` step, never a `?? default`
/// inside `run` (§0.2).
pub fn budget_of(req: &PassRequest, cfg: &crate::ReconConfig) -> usize {
    req.max_calls.unwrap_or(cfg.max_calls_per_pass)
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

    let budget = budget_of(req, &inner.cfg);
    let mut calls = 0usize;
    let mut tokens_in = 0u64;
    let mut tokens_out = 0u64;
    let mut appended: Vec<(bough_plugin_ledger::StepId, StepType)> = Vec::new();
    let mut contradictions = Vec::new();
    // Steps a confirmed contradiction made stale, and the claim that says so.
    let mut contradicted: Vec<(
        bough_plugin_ledger::StepId,
        StepType,
        bough_plugin_ledger::StepId,
    )> = Vec::new();

    for pair in &plan.contradiction_candidates {
        if calls >= budget {
            break;
        }
        let (answer, used) = judge(inner, req, &pass, &wake, pair).await?;
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
        contradicted.push((pair.older.clone(), pair.older_kind.clone(), step.id.clone()));
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
    for (step, kind, claim) in contradicted {
        // THE SECOND PIN LOCK, on the path that does not go through `detect::stale` (§3, V7). A
        // pin may be the older half of a confirmed contradiction — `pin/set` is `ClassRule::
        // Either`, so it can be EVIDENCE and `detect::pairs` will pair it — and the claim above
        // still stands. What must never happen is a `memory/expired` marker naming it: a pin's
        // only relief valve is supersession.
        if !bough_plugin_rollups::is_expirable(&kind) {
            continue;
        }
        if candidates.iter().any(|(c, _)| c.step == step) {
            continue;
        }
        candidates.push((
            Candidate {
                // The kind of the step being EXPIRED, not of the claim that justified it.
                step,
                kind,
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

    // The distilled block: ADDED through the rollups seam, never sealed here. It is a MODEL CALL
    // and is therefore inside `max_calls_per_pass` (P4-D15), not added on top of it.
    let distilled = if plan.distil && calls < budget {
        let outcome = inner
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
                parents: Vec::new(),
            })
            .await;
        match outcome {
            Ok(report) => {
                calls += report.calls;
                Some(report.digest)
            }
            // A seam that REFUSES to summarize (the `rollups-none` stub) is a composition
            // choice, not a pass failure: the contradictions and expiry markers this pass
            // already appended stand, and the report says no block was distilled. Anything
            // else — a ledger error, a bad block — still fails the pass.
            Err(bough_plugin_rollups::RollupsError::Refused(_)) => None,
            Err(e) => return Err(e.into()),
        }
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
async fn judge(
    inner: &ReconInner,
    req: &PassRequest,
    pass: &ReconPassId,
    wake: &WakeId,
    pair: &crate::Pair,
) -> Result<(String, (u64, u64)), ReconError> {
    let older = inner.ledger.0.step(&pair.older).await.ok().flatten();
    let newer = inner.ledger.0.step(&pair.newer).await.ok().flatten();
    let (Some(older), Some(newer)) = (older, newer) else {
        return Ok((String::new(), (0, 0)));
    };

    let facts = Arc::new(RequestFacts {
        agent: req.agent.clone(),
        traj: req.traj.clone(),
        // The RUNNING pass's wake, so a policy decision and a token count are greppable back to
        // the steps the pass wrote (P4-D2).
        wake: wake.clone(),
        // A governance pass is unattended work: `model-policy` reads exactly this and picks terra.
        wake_kind: WakeKind::Scheduled,
        step_index: 0,
        answers_andrey: false,
        model_override: None,
        prompt_ver: inner.cfg.judge_prompt_ver.clone(),
        composition: inner
            .ctx
            .kernel()
            .and_then(|k| k.composition())
            .map(|c| c.fingerprint.as_str().to_string())
            .unwrap_or_default(),
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

    if decided.call.model.is_empty() {
        // §12: choosing the model is `model-policy`'s, and a call with no model chosen is a
        // composition fault, not a cleared pair. The sibling row refuses identically.
        return Err(ReconError::Model(
            "no `agent/request` listener chose a model for the reconsolidation pass; \
             `model-policy` is what does that (§12)"
                .to_string(),
        ));
    }
    let system = crate::prompts::system(&inner.cfg.judge_prompt_ver)
        .expect("the row validated its judge prompt version at boot");
    let user = prompt(&older, &newer, &pair.shared);
    let model = decided.call.model.clone();
    let request = Arc::new(LlmRequest {
        model: model.clone(),
        system: Some(system.to_string()),
        system_volatile: None,
        messages: vec![LlmMessage {
            role: LlmRole::User,
            content: vec![LlmContentBlock::Text { text: user.clone() }],
        }],
        tools: vec![],
        call: decided.call,
    });

    let cancel = tokio_util::sync::CancellationToken::new();
    let mut stream = inner.llm.stream(&inner.ctx, request, cancel).await;
    let mut text = String::new();
    let mut used = (0u64, 0u64);
    let mut failed = false;
    while let Some(chunk) = stream.next().await {
        match chunk {
            Chunk::TextDelta { text: t } => text.push_str(&t),
            Chunk::Usage(u) => used = (u.input_tokens.max(0) as u64, u.output_tokens.max(0) as u64),
            // A failure is not a verdict: the pair clears. The CALL still happened, so it is
            // still recorded — a billed call the ledger cannot see is exactly what §0.2 forbids.
            Chunk::Failed(_) => {
                failed = true;
                text.clear();
                break;
            }
            Chunk::End { .. } => break,
            _ => {}
        }
    }

    // Model-visible ⟺ ledgered (§0.2): one `recon/request` per call, failed calls included.
    inner
        .ledger
        .0
        .append(Append {
            traj: req.traj.clone(),
            wake: wake.clone(),
            kind: StepType::new(RECON_REQUEST),
            class: Class::Thought,
            body: serde_json::to_value(ReconRequest {
                pass: pass.to_string(),
                prompt_ver: inner.cfg.judge_prompt_ver.clone(),
                model,
                older: older.id.to_string(),
                newer: newer.id.to_string(),
                input_digest: input_digest(&user),
                tokens_in: used.0,
                tokens_out: used.1,
                failed,
            })
            .expect("ReconRequest serialises"),
            cites: vec![],
            at: req.at,
            id: None,
        })
        .await?;

    Ok((text, used))
}

/// PURE: sha256 of the rendered judge input, hex. The replay proof a `recon/request` carries.
pub fn input_digest(user: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(user.as_bytes());
    format!("{:x}", h.finalize())
}

/// The judge's standing instruction at the version `bough-base` ships. The TEXT lives in
/// [`crate::prompts`], which is what `judge_prompt_ver` names: editing it without adding a
/// version is a change the catalog cannot express.
pub const SYSTEM: &str = crate::prompts::RECON_1_SYSTEM;

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
