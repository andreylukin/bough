//! Invariant (§8): a digest rebuild READS sealed tiers and writes none. It supersedes the previous
//! digest and repoints `agents.digest_rollup`; the tier count on the trajectory is unchanged
//! across it, which is what `/reset` relies on.

use std::sync::Arc;

use bough_plugin_ledger::{
    Class, NewRollup, Order, Rollup, RollupId, RollupKind, RollupQuery, Seq, SeqRange, Step,
    StepQuery,
};
use bough_plugin_rollups::{
    Cut, DigestBlock, DigestReport, DigestRequest, PassId, RollupsError, Standing, Window,
};

use crate::call::{self, CallRequest, Phase};
use crate::SummarizerInner;

/// The deterministic id of an agent's standing digest at generation `gen`.
///
/// Its own namespace, so [`bough_plugin_rollups::is_ours`] — which answers for TIER blocks — never
/// claims a digest, and `/supersede` cannot be pointed at one: a digest's relief valve is a
/// rebuild, not a supersession.
pub fn digest_id(traj: &bough_plugin_ledger::TrajId, gen: u32) -> RollupId {
    if gen == 0 {
        RollupId::new(format!("digest:{traj}"))
    } else {
        RollupId::new(format!("digest:{traj}#g{gen}"))
    }
}

/// Rebuild the standing digest. `from_raw` ignores the existing digest entirely.
pub async fn rebuild(
    inner: &SummarizerInner,
    req: &DigestRequest,
) -> Result<DigestReport, RollupsError> {
    let cfg = inner.cfg.clone();
    // Sealed tiers are READ. Nothing below writes one, and the report says how many were read so
    // a test can say the count did not move.
    let tiers: Vec<Rollup> = inner
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![req.traj.clone()],
            kind: Some(RollupKind::Tier),
            ..Default::default()
        })
        .await?;
    let tiers_read = tiers.len();

    let previous = match inner.ledger.0.agent(&req.agent).await? {
        Some(row) => match row.digest_rollup {
            Some(id) => crate::seal::find_rollup(inner, &id).await?,
            None => None,
        },
        None => None,
    };

    // §8's "rebuilds … from raw evidence": the raw steps are the SOURCE, and the sealed tiers are
    // read as an index over them. The sample is derived from the row's own shape, not from a
    // constant nobody can configure.
    let raw_limit = cfg.max_window_steps.saturating_mul(cfg.fanout).max(1);
    let mut raw: Vec<Step> = inner
        .ledger
        .0
        .steps(&StepQuery {
            trajs: vec![req.traj.clone()],
            class: Some(Class::Evidence),
            order: Order::SeqDesc,
            limit: Some(raw_limit),
            ..Default::default()
        })
        .await?;
    raw.sort_by_key(|s| s.seq);

    let head = inner.ledger.0.head_seq(&req.traj).await?.unwrap_or(Seq(0));
    let user = render_digest_input(&tiers, &raw, previous.as_ref(), req.from_raw);
    let pass = PassId::new(format!("pass:{}", uuid::Uuid::now_v7()));
    let wake = call::pass_wake(&pass);
    let mut facts = call::facts(&req.agent, &req.traj, &pass, &cfg, &inner.composition);
    facts.model_override = inner
        .ledger
        .0
        .agent(&req.agent)
        .await?
        .and_then(|row| row.model_override);
    let system = crate::prompts::system(Phase::Digest, &cfg.prompt_ver)
        .expect("the row validated its prompt version at boot");
    let outcome = call::call(
        &inner.ctx,
        &inner.llm,
        &inner.ledger,
        CallRequest {
            phase: Phase::Digest,
            facts: Arc::new(facts),
            system,
            user,
            max_tokens: cfg.reduce_max_tokens,
            tier: 0,
            range: SeqRange {
                from: Seq(1),
                to: Seq(head.0.max(1)),
            },
            at: req.at,
        },
    )
    .await?;

    let block = build_block(
        &outcome.text,
        &tiers,
        &raw,
        previous.as_ref().map(|r| r.id.clone()),
        &cfg,
    )?;
    let gen = previous
        .as_ref()
        .map(|r| crate::seal::generation_of(&r.id).saturating_add(1))
        .unwrap_or(0);
    let id = digest_id(&req.traj, gen);
    let sealed = inner
        .ledger
        .0
        .seal_rollup(NewRollup {
            id: Some(id),
            traj: req.traj.clone(),
            kind: RollupKind::Digest,
            // A digest is not a tier: it spans the whole trajectory and sits at tier 0, so the
            // degradation ladder's per-tier arithmetic never counts it as fine detail.
            tier: 0,
            // `Seq` starts at 1 (the ledger's own schema says so), so a whole-trajectory span is
            // 1..head, not 0..head.
            from_seq: Seq(1),
            to_seq: Seq(head.0.max(1)),
            src_trajs: vec![req.traj.clone()],
            body: serde_json::to_value(&block).expect("a DigestBlock serialises"),
            // A digest is the agent's OWN standing summary: notable to it and to nobody by ref,
            // which P1-D13 spells as the empty set.
            notable_refs: Default::default(),
            prompt_ver: cfg.prompt_ver.clone(),
            sealed_at: req.at,
        })
        .await?;

    announce_digest(inner, &wake, &sealed, &block, req.at).await?;

    if let Some(prev) = &previous {
        inner
            .ledger
            .0
            .supersede_rollup(&prev.id, &sealed.id)
            .await?;
    }
    // Identity renders from the agents row + the digest (§3), so repointing the row IS the
    // identity rebuild; there is nothing else to write.
    if let Some(mut row) = inner.ledger.0.agent(&req.agent).await? {
        row.digest_rollup = Some(sealed.id.clone());
        inner.ledger.0.put_agent(row).await?;
    }

    Ok(DigestReport {
        digest: sealed.id,
        replaced: previous.map(|r| r.id),
        tiers_read,
        calls: 1,
    })
}

/// The `rollup/sealed` step a digest gets, and its observation on the seam's stream.
async fn announce_digest(
    inner: &SummarizerInner,
    wake: &bough_plugin_ledger::WakeId,
    sealed: &Rollup,
    block: &DigestBlock,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), RollupsError> {
    // A digest's `beneath` is its `from_blocks` and its `evidence`; the tier vocabulary's
    // `refs_of` reads a `TierBlock`, so the digest states its own here rather than pretending.
    let tier_shaped = bough_plugin_rollups::TierBlock {
        text: block.text.clone(),
        themes: Vec::new(),
        beneath: bough_plugin_rollups::Beneath::Blocks {
            rollups: block.from_blocks.clone(),
        },
        evidence: block.evidence.clone(),
        windows: Vec::new(),
        tier: 0,
        prompt_ver: block.prompt_ver.clone(),
    };
    crate::seal::announce(inner, wake, sealed, &tier_shaped, at).await
}

/// PURE: what the digest call sees.
pub fn render_digest_input(
    tiers: &[Rollup],
    raw: &[Step],
    previous: Option<&Rollup>,
    from_raw: bool,
) -> String {
    let mut out = String::new();
    // `/reset` sets `from_raw`: the previous digest is not shown at all, so a drifted digest
    // cannot seed its own replacement (§8).
    if !from_raw {
        if let Some(p) = previous {
            out.push_str("the digest as it stands (may be stale):\n");
            out.push_str(&crate::render::block_text(p));
            out.push_str("\n\n");
        }
    }
    if !tiers.is_empty() {
        out.push_str(
            "sealed tier blocks (an index over the raw below; do not re-summarise them):\n",
        );
        out.push_str(&crate::render::render_children(tiers));
    }
    if !raw.is_empty() {
        let window = Window {
            from_seq: raw[0].seq,
            to_seq: raw[raw.len() - 1].seq,
            from_at: raw[0].at,
            to_at: raw[raw.len() - 1].at,
            steps: raw.iter().map(|s| s.id.clone()).collect(),
            cut: Cut::Head,
        };
        out.push_str("raw evidence:\n");
        out.push_str(&crate::render::render_window(raw, &window));
    }
    if out.is_empty() {
        out.push_str("this agent has no sealed tiers and no cited evidence yet.\n");
    }
    out
}

/// PURE: the answer, plus the index the model is not trusted with (P4-D17).
pub fn build_block(
    answer: &str,
    tiers: &[Rollup],
    raw: &[Step],
    replaces: Option<RollupId>,
    cfg: &crate::SummarizerConfig,
) -> Result<DigestBlock, RollupsError> {
    let parsed = crate::render::parse_block(
        answer,
        &bough_plugin_rollups::Inputs::Raw(raw.iter().map(|s| s.id.clone()).collect()),
        raw,
        cfg,
    )?;
    Ok(DigestBlock {
        text: parsed.text,
        standing: parsed
            .themes
            .into_iter()
            .map(|t| Standing {
                text: if t.title.is_empty() {
                    t.text
                } else {
                    format!("{}: {}", t.title, t.text)
                },
                evidence: t.evidence,
            })
            .collect(),
        evidence: parsed.evidence,
        from_blocks: tiers.iter().map(|r| r.id.clone()).collect(),
        replaces,
        prompt_ver: cfg.prompt_ver.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::TrajId;

    #[test]
    fn a_digest_id_carries_its_generation_and_is_not_a_tier_id() {
        let t = TrajId::new("lane/sol");
        assert_eq!(digest_id(&t, 0).as_str(), "digest:lane/sol");
        assert_eq!(digest_id(&t, 2).as_str(), "digest:lane/sol#g2");
        assert_eq!(crate::seal::generation_of(&digest_id(&t, 2)), 2);
    }

    /// §8: a reset must not let a drifted digest seed its own replacement.
    #[test]
    fn from_raw_hides_the_previous_digest() {
        let prev = Rollup {
            id: digest_id(&TrajId::new("t"), 0),
            traj: TrajId::new("t"),
            kind: RollupKind::Digest,
            tier: 0,
            from_seq: Seq(0),
            to_seq: Seq(9),
            src_trajs: vec![],
            body: serde_json::json!({ "text": "I am the stale digest" }),
            notable_refs: Default::default(),
            prompt_ver: "r4.1".into(),
            sealed_at: chrono::Utc::now(),
            superseded_by: None,
        };
        let with = render_digest_input(&[], &[], Some(&prev), false);
        let without = render_digest_input(&[], &[], Some(&prev), true);
        assert!(with.contains("I am the stale digest"));
        assert!(!without.contains("I am the stale digest"));
        assert!(without.contains("no sealed tiers"), "{without}");
    }
}
