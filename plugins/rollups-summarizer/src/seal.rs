//! Invariant: the pass appends, and only appends. Every block is written with `seal_rollup` under
//! a synthetic pass wake (P4-D2), one `rollup/request` per model call and one `rollup/sealed` per
//! block; nothing above `upto` is touched, and nothing within `seal_lag_steps` of the head is
//! sealed (P4-D11), so a sealed tier and the verbatim tail never describe the same steps.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_ledger::{
    AgentName, Append, Class, NewRollup, Order, Ref, Rollup, RollupId, RollupKind, RollupQuery,
    Seq, SeqRange, Step, StepId, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_llm::RequestFacts;
use bough_plugin_rollups::{
    block, plan as planner, windows, Cut, Inputs, PassId, PlannedBlock, RollupsError, SealPlan,
    SealReport, SealRequest, Skip, SkipReason, Stop, SupersedeReport, SupersedeRequest, TierBlock,
    Window,
};

use crate::call::{self, CallRequest, Phase};
use crate::{MemoryExpired, SummarizerInner, EXPIRY_KIND_SUPERSESSION, MEMORY_EXPIRED};

/// The generation encoded in a block id (`…#g2` ⇒ 2; no suffix ⇒ 0).
///
/// A local read of the namespace WP-1's [`planner::tier_id`] writes, so supersession can mint n+1
/// without a second source of truth about the FORMAT — `tier_id` still mints every id.
pub fn generation_of(id: &RollupId) -> u32 {
    // WP-1 answers for the `tier:` namespace, which is where the format lives. A DIGEST id is not
    // in it, and a digest still has a generation, so the same suffix is read off it here.
    planner::generation_of(id).unwrap_or_else(|| {
        id.as_str()
            .rsplit_once("#g")
            .and_then(|(_, n)| n.parse::<u32>().ok())
            .unwrap_or(0)
    })
}

/// The trajectory's MATERIAL steps: everything a governance pass did not itself write.
///
/// A pass appends `rollup/request` and `rollup/sealed` to the very trajectory it reads, so without
/// this filter a summarizer would window its own bookkeeping into episodes and summarize its own
/// request log — and, worse, each pass would move the head and re-open the range below the lag,
/// which is exactly what "a second pass over an unchanged ledger seals nothing" forbids
/// (P4-D19, recorded in the WP-2 report).
async fn material(inner: &SummarizerInner, traj: &TrajId) -> Result<Vec<Step>, RollupsError> {
    let steps = inner
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?;
    Ok(steps
        .into_iter()
        .filter(|s| !crate::GOVERNANCE_KINDS.contains(&s.kind.as_str()))
        .collect())
}

/// Every rollup on the trajectory, superseded ones INCLUDED: a superseded range is still sealed
/// and is never re-planned.
async fn existing(inner: &SummarizerInner, traj: &TrajId) -> Result<Vec<Rollup>, RollupsError> {
    Ok(inner
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![traj.clone()],
            include_superseded: true,
            ..Default::default()
        })
        .await?)
}

/// The seq a pass may seal up to. `None` on the request ⇒ `head - seal_lag_steps` (P4-D11), and a
/// caller-supplied `upto` never RAISES that ceiling: the lag is the row's rule, not a suggestion.
/// The call budget for one pass. A named `resolve(request) -> Spec` step beside `upto_of`, never
/// a `?? default` inside `run` (§0.2).
fn budget_of(req: &SealRequest, cfg: &crate::SummarizerConfig) -> usize {
    req.max_calls.unwrap_or(cfg.max_calls_per_pass)
}

fn upto_of(req: &SealRequest, head: Seq, lag: usize) -> Seq {
    let lagged = Seq(head.0.saturating_sub(lag as u64));
    match req.upto {
        Some(u) => Seq(u.0.min(lagged.0)),
        None => lagged,
    }
}

/// Plan a pass: pure with respect to the world (reads the ledger, calls no model, writes nothing).
pub async fn plan(inner: &SummarizerInner, req: &SealRequest) -> Result<SealPlan, RollupsError> {
    let cfg = inner.cfg.clone();
    let tcfg = crate::resolve::tier_cfg(&cfg);
    let wcfg = crate::resolve::window_cfg(&cfg);
    // The head is the last MATERIAL seq, not the store's: a pass's own steps must not push the
    // lag ceiling forward and re-open the range beneath it.
    let all = material(inner, &req.traj).await?;
    let head = all.last().map(|s| s.seq).unwrap_or(Seq(0));
    let upto = upto_of(req, head, cfg.seal_lag_steps);
    let steps: Vec<Step> = all.into_iter().filter(|s| s.seq <= upto).collect();
    let ws = windows(&steps, &wcfg);
    let have = existing(inner, &req.traj).await?;
    Ok(planner::plan(
        &have, &ws, head, upto, &req.traj, &tcfg, &wcfg,
    ))
}

/// Run a pass to its budget.
pub async fn run(inner: &SummarizerInner, req: &SealRequest) -> Result<SealReport, RollupsError> {
    let cfg = inner.cfg.clone();
    let pass = PassId::new(format!("pass:{}", uuid::Uuid::now_v7()));
    let wake = call::pass_wake(&pass);
    let sealplan = plan(inner, req).await?;
    let mut report = SealReport {
        pass: pass.clone(),
        planned: sealplan.blocks.len(),
        sealed: Vec::new(),
        skipped: sealplan.skipped.clone(),
        calls: 0,
        tokens_in: 0,
        tokens_out: 0,
        stop: Stop::NothingToDo,
    };
    if sealplan.blocks.is_empty() {
        // Idempotence, said plainly: a second pass over an unchanged ledger seals nothing and
        // SAYS nothing-to-do rather than reporting a successful empty pass.
        return Ok(report);
    }

    let max_calls = budget_of(req, &cfg);
    let facts = Arc::new(governance_facts(inner, &req.agent, &req.traj, &pass).await?);
    let all: Vec<Step> = material(inner, &req.traj)
        .await?
        .into_iter()
        .filter(|s| s.seq <= sealplan.upto)
        .collect();

    report.stop = Stop::Complete;
    for planned in &sealplan.blocks {
        if report.calls >= max_calls {
            report.stop = Stop::CallBudget;
            report.skipped.push(Skip {
                tier: planned.tier,
                from_seq: planned.from_seq,
                to_seq: planned.to_seq,
                why: SkipReason::CallBudget,
            });
            continue;
        }
        let budget_spent = report.stop == Stop::CallBudget;
        match seal_one(inner, req, &facts, &wake, planned, &all).await? {
            Some(sealed) => {
                report.calls += 1;
                report.tokens_in += sealed.tokens_in;
                report.tokens_out += sealed.tokens_out;
                report.sealed.push(sealed.id);
            }
            // A parent whose children are not all sealed. WHY they are not is the operator's
            // question: when this pass ran out of calls below it, the cause is the budget, and
            // saying "not enough children" would send them looking at `fanout` instead.
            None => report.skipped.push(Skip {
                tier: planned.tier,
                from_seq: planned.from_seq,
                to_seq: planned.to_seq,
                why: if budget_spent {
                    SkipReason::CallBudget
                } else {
                    SkipReason::NotEnoughChildren
                },
            }),
        }
    }
    Ok(report)
}

/// The facts every call of one pass shares. `model_override` is the agent's own, read from the
/// ledger row exactly as §12 says an unattended wake reads it.
async fn governance_facts(
    inner: &SummarizerInner,
    agent: &AgentName,
    traj: &TrajId,
    pass: &PassId,
) -> Result<RequestFacts, RollupsError> {
    let mut facts = call::facts(agent, traj, pass, &inner.cfg, &inner.composition);
    facts.model_override = inner
        .ledger
        .0
        .agent(agent)
        .await?
        .and_then(|row| row.model_override);
    Ok(facts)
}

/// What one sealed block cost.
struct Sealed {
    id: RollupId,
    tokens_in: u64,
    tokens_out: u64,
}

/// Seal exactly one planned block. `Ok(None)` when its inputs are no longer resolvable.
async fn seal_one(
    inner: &SummarizerInner,
    req: &SealRequest,
    facts: &Arc<RequestFacts>,
    wake: &WakeId,
    planned: &PlannedBlock,
    all: &[Step],
) -> Result<Option<Sealed>, RollupsError> {
    let cfg = inner.cfg.clone();
    // The steps the block COVERS, whatever layer it reduces: the notable refs are a property of
    // the covered raw material, never of the model's answer (P4-D17).
    let covered: Vec<Step> = all
        .iter()
        .filter(|s| s.seq >= planned.from_seq && s.seq <= planned.to_seq)
        .cloned()
        .collect();

    let (phase, user, max_tokens) = match &planned.inputs {
        Inputs::Raw(_) => {
            let mut user = String::new();
            for w in &planned.windows {
                user.push_str(&crate::render::render_window(all, w));
                user.push('\n');
            }
            (Phase::Map, user, cfg.map_max_tokens)
        }
        Inputs::Blocks(ids) => {
            let have = existing(inner, &req.traj).await?;
            let children: Vec<Rollup> = ids
                .iter()
                .filter_map(|id| have.iter().find(|r| &r.id == id).cloned())
                .collect();
            if children.len() != ids.len() {
                return Ok(None);
            }
            (
                Phase::Reduce,
                crate::render::render_children(&children),
                cfg.reduce_max_tokens,
            )
        }
    };

    // THE BELT to the planner's braces, BEFORE the call: the planner refuses a sealed range
    // before the model is ever reached, and this refuses it again over a store that may have
    // moved since the plan was made. Below the call it would still refuse, but only after paying
    // for a block it then throws away — and the `?` would lose the report for everything this
    // pass already sealed.
    refuse_if_sealed(inner, &req.traj, planned).await?;

    let system = crate::prompts::system(phase, &cfg.prompt_ver).ok_or_else(|| {
        RollupsError::BadBlock(format!(
            "no {phase:?} prompt at version `{}`; the row should not have booted",
            cfg.prompt_ver
        ))
    })?;
    let outcome = call::call(
        &inner.ctx,
        &inner.llm,
        &inner.ledger,
        CallRequest {
            phase,
            facts: facts.clone(),
            system,
            user,
            max_tokens,
            tier: planned.tier,
            range: SeqRange {
                from: planned.from_seq,
                to: planned.to_seq,
            },
            at: req.at,
        },
    )
    .await?;

    let mut block = crate::render::parse_block(&outcome.text, &planned.inputs, &covered, &cfg)?;
    crate::render::stamp(&mut block, planned.tier, &planned.windows);
    // A coarse block resolves to RAW in one hop (P4-D5): above tier 1 the evidence is drawn from
    // the children's own evidence, bounded, rather than from the whole covered range.
    if matches!(planned.inputs, Inputs::Blocks(_)) {
        block.evidence =
            coarse_evidence(inner, &req.traj, &planned.inputs, cfg.max_evidence_refs).await?;
    }

    let id = write_block(
        inner,
        &req.traj,
        wake,
        planned.id.clone(),
        planned.tier,
        planned.from_seq,
        planned.to_seq,
        &block,
        &covered,
        req.at,
    )
    .await?;
    Ok(Some(Sealed {
        id,
        tokens_in: outcome.tokens_in,
        tokens_out: outcome.tokens_out,
    }))
}

/// Refuse a planned block whose range is already covered at its tier by a live block of ours.
///
/// `Err(RollupsError::AlreadySealed)` names the existing block, so a caller never has to guess
/// whether a seal was skipped, refused or lost.
pub async fn refuse_if_sealed(
    inner: &SummarizerInner,
    traj: &TrajId,
    planned: &PlannedBlock,
) -> Result<(), RollupsError> {
    let have = existing(inner, traj).await?;
    let clash = have.iter().find(|r| {
        r.kind == RollupKind::Tier
            && &r.traj == traj
            && r.tier == planned.tier
            && r.from_seq == planned.from_seq
            && r.to_seq == planned.to_seq
            && planner::is_ours(&r.id)
    });
    match clash {
        None => Ok(()),
        Some(r) => Err(RollupsError::AlreadySealed {
            traj: traj.clone(),
            tier: planned.tier,
            from: planned.from_seq,
            to: planned.to_seq,
            existing: r.id.clone(),
        }),
    }
}

/// The raw evidence a tier k>1 block carries: its children's own evidence, deduplicated in child
/// order and capped, so a projected coarse block still names raw steps.
async fn coarse_evidence(
    inner: &SummarizerInner,
    traj: &TrajId,
    inputs: &Inputs,
    max: usize,
) -> Result<Vec<StepId>, RollupsError> {
    let Inputs::Blocks(ids) = inputs else {
        return Ok(Vec::new());
    };
    let have = existing(inner, traj).await?;
    let mut out: Vec<StepId> = Vec::new();
    let mut seen: BTreeSet<StepId> = BTreeSet::new();
    for id in ids {
        let Some(child) = have.iter().find(|r| &r.id == id) else {
            continue;
        };
        let evidence = serde_json::from_value::<TierBlock>(child.body.clone())
            .map(|b| b.evidence)
            .unwrap_or_default();
        for step in evidence {
            if out.len() >= max {
                return Ok(out);
            }
            if seen.insert(step.clone()) {
                out.push(step);
            }
        }
    }
    Ok(out)
}

/// Seal the row, append `rollup/sealed`, and record the observation the seam's invariant judges.
#[allow(clippy::too_many_arguments)]
async fn write_block(
    inner: &SummarizerInner,
    traj: &TrajId,
    wake: &WakeId,
    id: RollupId,
    tier: u8,
    from: Seq,
    to: Seq,
    block: &TierBlock,
    covered: &[Step],
    at: chrono::DateTime<chrono::Utc>,
) -> Result<RollupId, RollupsError> {
    let cfg = inner.cfg.clone();
    let sealed = inner
        .ledger
        .0
        .seal_rollup(NewRollup {
            id: Some(id),
            traj: traj.clone(),
            kind: RollupKind::Tier,
            tier,
            from_seq: from,
            to_seq: to,
            src_trajs: vec![traj.clone()],
            body: serde_json::to_value(block).expect("a TierBlock serialises"),
            // P1-D13: a real set when the covered steps carry refs, and EMPTY — "notable to
            // everyone" — only when they carry none.
            notable_refs: block::notable_refs(covered, cfg.max_notable_refs),
            prompt_ver: cfg.prompt_ver.clone(),
            sealed_at: at,
        })
        .await?;
    announce(inner, wake, &sealed, at).await?;
    Ok(sealed.id)
}

/// The `rollup/sealed` step a sealed row gets. The seam's invariants read the ROW, not this step,
/// so a sealed block is never invisible to them whatever a provider announces.
pub async fn announce(
    inner: &SummarizerInner,
    wake: &WakeId,
    sealed: &Rollup,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), RollupsError> {
    inner
        .ledger
        .0
        .append(Append {
            traj: sealed.traj.clone(),
            wake: wake.clone(),
            kind: StepType::new("rollup/sealed"),
            class: Class::Evidence,
            body: serde_json::to_value(bough_plugin_ledger::vocabulary::RollupSealed {
                rollup: sealed.id.clone(),
                kind: sealed.kind,
                tier: sealed.tier,
                from_seq: sealed.from_seq,
                to_seq: sealed.to_seq,
                prompt_ver: sealed.prompt_ver.clone(),
            })
            .expect("RollupSealed serialises"),
            cites: vec![call::rollup_cite(&sealed.id)],
            at,
            id: None,
        })
        .await?;
    Ok(())
}

/// Supersede one block at generation n+1 and append the `memory/expired` note naming the old one.
pub async fn supersede(
    inner: &SummarizerInner,
    req: &SupersedeRequest,
) -> Result<SupersedeReport, RollupsError> {
    if !planner::is_ours(&req.block) {
        return Err(RollupsError::NotOurs(req.block.clone()));
    }
    let old = find_rollup(inner, &req.block)
        .await?
        .ok_or_else(|| RollupsError::NotFound(req.block.clone()))?;
    if let Some(by) = &old.superseded_by {
        // §3's one set-once write, said once: a second supersession is a refusal, not a no-op.
        return Err(RollupsError::AlreadySuperseded(old.id.clone(), by.clone()));
    }

    let gen = generation_of(&old.id).saturating_add(1);
    let new_id = planner::tier_id(&old.traj, old.tier, old.from_seq, old.to_seq, gen);
    let agent = agent_of(inner, &old.traj).await?;
    let pass = PassId::new(format!("pass:{}", uuid::Uuid::now_v7()));
    let wake = call::pass_wake(&pass);
    let facts = Arc::new(governance_facts(inner, &agent, &old.traj, &pass).await?);
    let covered = covered_steps(inner, &old.traj, old.from_seq, old.to_seq).await?;

    // A suspected-bad block is RE-SUMMARIZED INTO A NEW BLOCK, never edited in place (§8).
    let inputs = Inputs::Raw(covered.iter().map(|s| s.id.clone()).collect());
    let window = Window {
        from_seq: old.from_seq,
        to_seq: old.to_seq,
        from_at: covered.first().map(|s| s.at).unwrap_or(req.at),
        to_at: covered.last().map(|s| s.at).unwrap_or(req.at),
        steps: covered.iter().map(|s| s.id.clone()).collect(),
        cut: Cut::Gap,
    };
    let system = crate::prompts::system(Phase::Map, &inner.cfg.prompt_ver)
        .expect("the row validated its prompt version at boot");
    let outcome = call::call(
        &inner.ctx,
        &inner.llm,
        &inner.ledger,
        CallRequest {
            phase: Phase::Map,
            facts,
            system,
            user: crate::render::render_window(&covered, &window),
            max_tokens: inner.cfg.map_max_tokens,
            tier: old.tier,
            range: SeqRange {
                from: old.from_seq,
                to: old.to_seq,
            },
            at: req.at,
        },
    )
    .await?;
    let mut block = crate::render::parse_block(&outcome.text, &inputs, &covered, &inner.cfg)?;
    crate::render::stamp(&mut block, old.tier, std::slice::from_ref(&window));

    let new = write_block(
        inner,
        &old.traj,
        &wake,
        new_id,
        old.tier,
        old.from_seq,
        old.to_seq,
        &block,
        &covered,
        req.at,
    )
    .await?;
    inner.ledger.0.supersede_rollup(&old.id, &new).await?;
    let note = expiry_note(inner, &old.traj, &wake, &old.id, &req.reason, req.at).await?;
    Ok(SupersedeReport {
        old: old.id.clone(),
        new,
        note,
    })
}

/// The APPENDED marker a supersession leaves (§8). Evidence, so the ledger itself refuses one
/// that cannot say what it is about.
pub async fn expiry_note(
    inner: &SummarizerInner,
    traj: &TrajId,
    wake: &WakeId,
    old: &RollupId,
    reason: &str,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<StepId, RollupsError> {
    let step = inner
        .ledger
        .0
        .append(Append {
            traj: traj.clone(),
            wake: wake.clone(),
            kind: StepType::new(MEMORY_EXPIRED),
            class: Class::Evidence,
            body: serde_json::to_value(MemoryExpired {
                targets: vec![Ref::new(format!("rollup:{old}"))],
                reason: reason.to_string(),
                kind: EXPIRY_KIND_SUPERSESSION,
            })
            .expect("MemoryExpired serialises"),
            cites: vec![call::rollup_cite(old)],
            at,
            id: None,
        })
        .await?;
    Ok(step.id)
}

/// One rollup by id, superseded ones included.
pub async fn find_rollup(
    inner: &SummarizerInner,
    id: &RollupId,
) -> Result<Option<Rollup>, RollupsError> {
    Ok(inner
        .ledger
        .0
        .rollups(&RollupQuery {
            include_superseded: true,
            ..Default::default()
        })
        .await?
        .into_iter()
        .find(|r| &r.id == id))
}

/// The steps a seq range covers, inclusive at both ends.
pub async fn covered_steps(
    inner: &SummarizerInner,
    traj: &TrajId,
    from: Seq,
    to: Seq,
) -> Result<Vec<Step>, RollupsError> {
    Ok(inner
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            after: Some(Seq(from.0.saturating_sub(1))),
            before: Some(Seq(to.0.saturating_add(1))),
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?)
}

/// Which agent a trajectory belongs to. A supersession names a block, not an agent, and the
/// governance facts need one; an unrouted trajectory falls back to its own id, which matches no
/// `agents` row rather than borrowing someone else's `model_override`.
pub async fn agent_of(inner: &SummarizerInner, traj: &TrajId) -> Result<AgentName, RollupsError> {
    Ok(inner
        .ledger
        .0
        .agents()
        .await?
        .into_iter()
        .find(|a| &a.traj == traj)
        .map(|a| a.name)
        .unwrap_or_else(|| AgentName::new(traj.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_rollups::SealRequest;

    fn req(upto: Option<Seq>) -> SealRequest {
        SealRequest {
            agent: AgentName::new("sol"),
            traj: TrajId::new("t"),
            at: chrono::Utc::now(),
            upto,
            max_calls: None,
            attribution: bough_plugin_rollups::Attribution::System,
        }
    }

    /// P4-D11 is a CEILING, not a suggestion: a caller cannot ask a pass to seal into the tail.
    #[test]
    fn a_caller_supplied_upto_never_raises_the_lag_ceiling() {
        assert_eq!(upto_of(&req(None), Seq(100), 20), Seq(80));
        assert_eq!(upto_of(&req(Some(Seq(50))), Seq(100), 20), Seq(50));
        assert_eq!(upto_of(&req(Some(Seq(99))), Seq(100), 20), Seq(80));
        // A trajectory shorter than the lag seals nothing at all.
        assert_eq!(upto_of(&req(None), Seq(5), 20), Seq(0));
    }

    #[test]
    fn a_generation_suffix_is_read_back_out_of_the_id() {
        assert_eq!(generation_of(&RollupId::new("tier:t:1:1-10")), 0);
        assert_eq!(generation_of(&RollupId::new("tier:t:1:1-10#g1")), 1);
        assert_eq!(generation_of(&RollupId::new("tier:t:1:1-10#g12")), 12);
        // A foreign namespace has no generation, and saying 0 is right: it has never been
        // superseded by this provider.
        assert_eq!(generation_of(&RollupId::new("old-feed:nodes:7")), 0);
    }
}
