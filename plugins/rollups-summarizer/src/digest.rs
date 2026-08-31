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

/// The id of an INHERITANCE digest (§3): a digest of the parent chain, held BY the child. Its own
/// namespace again, so the child's standing digest and what it inherited are two rows and neither
/// supersedes the other.
pub fn inheritance_id(traj: &bough_plugin_ledger::TrajId, gen: u32) -> RollupId {
    if gen == 0 {
        RollupId::new(format!("digest:{traj}:inherited"))
    } else {
        RollupId::new(format!("digest:{traj}:inherited#g{gen}"))
    }
}

/// The id of a RECONCILIATION digest (P5-D13): the one block a merge produces over the two
/// trajectories it joined. A third namespace, for the same reason as the second — a reconciliation
/// is not the head's standing digest and neither supersedes the other — and the kind on the row is
/// [`RollupKind::Reconciliation`], so `graph-ops` can find it by kind alone.
pub fn recon_id(traj: &bough_plugin_ledger::TrajId, gen: u32) -> RollupId {
    if gen == 0 {
        RollupId::new(format!("recon:{traj}"))
    } else {
        RollupId::new(format!("recon:{traj}#g{gen}"))
    }
}

/// PURE: which trajectories a rebuild reads raw evidence from, and which id namespace it writes.
/// The one place the standing/inheritance split is decided (§0.2: `resolve(request) -> Spec`).
pub struct DigestSpec {
    /// The trajectories the raw evidence comes from.
    pub sources: Vec<bough_plugin_ledger::TrajId>,
    /// `src_trajs` on the sealed row: what this digest is a digest OF.
    pub src_trajs: Vec<bough_plugin_ledger::TrajId>,
    pub inheritance: bool,
    /// P5-D13: the same two-parent input, in the `recon:` namespace and at
    /// [`RollupKind::Reconciliation`]. `false` everywhere but a merge.
    pub reconcile: bool,
}

impl DigestSpec {
    /// The id namespace this spec writes in, at generation `gen`.
    pub fn id(&self, traj: &bough_plugin_ledger::TrajId, gen: u32) -> RollupId {
        if self.reconcile {
            recon_id(traj, gen)
        } else if self.inheritance {
            inheritance_id(traj, gen)
        } else {
            digest_id(traj, gen)
        }
    }

    /// The id prefix of the live block this spec would replace.
    pub fn prefix(&self, traj: &bough_plugin_ledger::TrajId) -> String {
        if self.reconcile {
            format!("recon:{traj}")
        } else if self.inheritance {
            format!("digest:{traj}:inherited")
        } else {
            format!("digest:{traj}")
        }
    }

    /// The kind the sealed row carries.
    pub fn kind(&self) -> RollupKind {
        if self.reconcile {
            RollupKind::Reconciliation
        } else {
            RollupKind::Digest
        }
    }
}

pub fn spec_of(req: &DigestRequest) -> DigestSpec {
    if req.parents.is_empty() {
        DigestSpec {
            sources: vec![req.traj.clone()],
            src_trajs: vec![req.traj.clone()],
            inheritance: false,
            reconcile: false,
        }
    } else {
        DigestSpec {
            sources: req.parents.clone(),
            src_trajs: req.parents.clone(),
            inheritance: true,
            reconcile: req.reconcile,
        }
    }
}

/// Rebuild the standing digest. `from_raw` ignores the existing digest entirely.
pub async fn rebuild(
    inner: &SummarizerInner,
    req: &DigestRequest,
) -> Result<DigestReport, RollupsError> {
    let cfg = inner.cfg.clone();
    let spec = spec_of(req);
    // Sealed tiers are READ. Nothing below writes one, and the report says how many were read so
    // a test can say the count did not move.
    let all_tiers: Vec<Rollup> = inner
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: spec.sources.clone(),
            kind: Some(RollupKind::Tier),
            ..Default::default()
        })
        .await?;
    let tiers_read = all_tiers.len();
    // §8: `/reset` rebuilds from RAW EVIDENCE, and "sealed tiers are never re-summarized by it".
    // `from_raw` therefore hides the tiers from the prompt as well as the previous digest — a
    // rebuild that reads the tier prose is a summary of the tiers, not a rebuild from the raw.
    let tiers: Vec<Rollup> = if req.from_raw { Vec::new() } else { all_tiers };

    // The block this rebuild replaces. A STANDING digest's predecessor is the one the agents row
    // points at; an INHERITANCE digest's is the live row in its own namespace, which no agents
    // row names. Reading it back from the store — rather than from the agents row alone — is
    // also what stops a rebuild whose repoint never happened from re-minting generation 0 over
    // an id that is already sealed (`ledger-sqlite` refuses the duplicate; `ledger-memory` used
    // to replace it silently).
    let previous = if spec.inheritance {
        live_in_namespace(inner, req, &spec).await?
    } else {
        let by_row = match inner.ledger.0.agent(&req.agent).await? {
            Some(row) => match row.digest_rollup {
                Some(id) => crate::seal::find_rollup(inner, &id).await?,
                None => None,
            },
            None => None,
        };
        match by_row {
            Some(r) => Some(r),
            None => live_in_namespace(inner, req, &spec).await?,
        }
    };

    // §8's "rebuilds … from raw evidence": the raw steps are the SOURCE, and the sealed tiers are
    // read as an index over them. The sample is derived from the row's own shape, not from a
    // constant nobody can configure.
    let raw_limit = cfg.max_window_steps.saturating_mul(cfg.fanout).max(1);
    let mut raw: Vec<Step> = inner
        .ledger
        .0
        .steps(&StepQuery {
            trajs: spec.sources.clone(),
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
    let id = spec.id(&req.traj, gen);
    let sealed = inner
        .ledger
        .0
        .seal_rollup(NewRollup {
            id: Some(id),
            traj: req.traj.clone(),
            kind: spec.kind(),
            // A digest is not a tier: it spans the whole trajectory and sits at tier 0, so the
            // degradation ladder's per-tier arithmetic never counts it as fine detail.
            tier: 0,
            // `Seq` starts at 1 (the ledger's own schema says so), so a whole-trajectory span is
            // 1..head, not 0..head.
            from_seq: Seq(1),
            to_seq: Seq(head.0.max(1)),
            // §3: `src_trajs` names what the digest is a digest OF — the agent's own trajectory
            // for a standing digest, the parent chain for an inheritance digest.
            src_trajs: spec.src_trajs.clone(),
            body: serde_json::to_value(&block).expect("a DigestBlock serialises"),
            // A digest is the agent's OWN standing summary: notable to it and to nobody by ref,
            // which P1-D13 spells as the empty set.
            notable_refs: Default::default(),
            prompt_ver: cfg.prompt_ver.clone(),
            sealed_at: req.at,
        })
        .await?;

    announce_digest(inner, &wake, &sealed, req.at).await?;

    if let Some(prev) = &previous {
        inner
            .ledger
            .0
            .supersede_rollup(&prev.id, &sealed.id)
            .await?;
    }
    // Identity renders from the agents row + the digest (§3), so repointing the row IS the
    // identity rebuild; there is nothing else to write. An INHERITANCE digest is not the agent's
    // standing digest and never repoints the row.
    if !spec.inheritance {
        if let Some(mut row) = inner.ledger.0.agent(&req.agent).await? {
            row.digest_rollup = Some(sealed.id.clone());
            inner.ledger.0.put_agent(row).await?;
        }
    }

    Ok(DigestReport {
        digest: sealed.id,
        replaced: previous.map(|r| r.id),
        tiers_read,
        calls: 1,
    })
}

/// The live (never-superseded) digest in one namespace on this trajectory. The belt to the agents
/// row's braces: a rebuild whose repoint failed still sees its own predecessor.
async fn live_in_namespace(
    inner: &SummarizerInner,
    req: &DigestRequest,
    spec: &DigestSpec,
) -> Result<Option<Rollup>, RollupsError> {
    let rows = inner
        .ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![req.traj.clone()],
            kind: Some(spec.kind()),
            include_superseded: true,
            ..Default::default()
        })
        .await?;
    let prefix = spec.prefix(&req.traj);
    let inheritance = spec.inheritance;
    Ok(rows
        .into_iter()
        .filter(|r| {
            r.superseded_by.is_none()
                && r.id.as_str().starts_with(&prefix)
                // `digest:<t>` is a prefix of `digest:<t>:inherited`; the standing namespace must
                // not claim the inheritance rows.
                && (inheritance
                    || !r.id.as_str().starts_with(&format!("digest:{}:", req.traj)))
        })
        .max_by_key(|r| crate::seal::generation_of(&r.id)))
}

/// The `rollup/sealed` step a digest gets.
async fn announce_digest(
    inner: &SummarizerInner,
    wake: &bough_plugin_ledger::WakeId,
    sealed: &Rollup,
    at: chrono::DateTime<chrono::Utc>,
) -> Result<(), RollupsError> {
    crate::seal::announce(inner, wake, sealed, at).await
}

/// PURE: what the digest call sees.
pub fn render_digest_input(
    tiers: &[Rollup],
    raw: &[Step],
    previous: Option<&Rollup>,
    from_raw: bool,
) -> String {
    // `/reset` sets `from_raw`: NEITHER the previous digest nor the sealed tiers are shown, so
    // the rebuild rests on raw evidence alone (§8). The caller already passes an empty tier list
    // in that case; this is the belt to those braces.
    let tiers: &[Rollup] = if from_raw { &[] } else { tiers };
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

    /// P5-D13: three namespaces, three kinds, and none of them a prefix trap for the others.
    #[test]
    fn a_reconciliation_is_its_own_namespace_and_kind() {
        let t = TrajId::new("lane/sol+lane/terra");
        assert_eq!(recon_id(&t, 0).as_str(), "recon:lane/sol+lane/terra");
        assert_eq!(recon_id(&t, 3).as_str(), "recon:lane/sol+lane/terra#g3");
        assert_eq!(crate::seal::generation_of(&recon_id(&t, 3)), 3);

        let recon = spec_of(&bough_plugin_rollups::DigestRequest {
            agent: bough_plugin_ledger::AgentName::new("sol"),
            traj: t.clone(),
            at: chrono::Utc::now(),
            attribution: bough_plugin_rollups::Attribution::Andrey,
            from_raw: false,
            parents: vec![TrajId::new("lane/sol"), TrajId::new("lane/terra")],
            reconcile: true,
        });
        assert!(recon.reconcile && recon.inheritance);
        assert_eq!(recon.kind(), RollupKind::Reconciliation);
        assert_eq!(recon.id(&t, 0), recon_id(&t, 0));
        // The same two-parent input WITHOUT the flag is an inheritance digest: one field, one
        // difference, and the sources are identical either way.
        let inherited = spec_of(&bough_plugin_rollups::DigestRequest {
            agent: bough_plugin_ledger::AgentName::new("sol"),
            traj: t.clone(),
            at: chrono::Utc::now(),
            attribution: bough_plugin_rollups::Attribution::Andrey,
            from_raw: false,
            parents: vec![TrajId::new("lane/sol"), TrajId::new("lane/terra")],
            reconcile: false,
        });
        assert_eq!(inherited.sources, recon.sources);
        assert_eq!(inherited.kind(), RollupKind::Digest);
        assert_eq!(inherited.id(&t, 0), inheritance_id(&t, 0));
        assert_ne!(inherited.prefix(&t), recon.prefix(&t));
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

    /// §8: "the reset rebuilds the digest … from raw evidence; sealed tiers are never
    /// re-summarized by it". `from_raw` therefore hides the TIERS as well as the previous digest
    /// — a rebuild that reads the tier prose is a summary of tiers, not a rebuild from the raw.
    #[test]
    fn from_raw_hides_the_sealed_tiers_too() {
        let tier = Rollup {
            id: RollupId::new("tier:t:1:1-10"),
            traj: TrajId::new("t"),
            kind: RollupKind::Tier,
            tier: 1,
            from_seq: Seq(1),
            to_seq: Seq(10),
            src_trajs: vec![],
            body: serde_json::json!({ "text": "I am a sealed tier block" }),
            notable_refs: Default::default(),
            prompt_ver: "r4.1".into(),
            sealed_at: chrono::Utc::now(),
            superseded_by: None,
        };
        let ordinary = render_digest_input(std::slice::from_ref(&tier), &[], None, false);
        assert!(ordinary.contains("I am a sealed tier block"));
        // A `/reset` never passes the tiers in at all, so `render_digest_input` is given none.
        // This is the belt: even handed them, `from_raw` must not render them.
        let reset = render_digest_input(std::slice::from_ref(&tier), &[], None, true);
        assert!(
            !reset.contains("I am a sealed tier block"),
            "a reset re-summarised a sealed tier: {reset}"
        );
    }
}
