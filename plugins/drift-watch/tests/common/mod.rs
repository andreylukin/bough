//! The harness these three suites share: a REAL store (`ledger-memory`, the behavioural twin of
//! `ledger-sqlite`) and a FAKE `Summarizer` written to the seam's contract.
//!
//! It is a fake and not `rollups-summarizer` on purpose. These suites judge THIS row — that the
//! reset asks for `from_raw`, cites raw steps, leaves the intent half empty and writes no sealed
//! tier — and none of that should be able to fail because a model, a replay fixture or another
//! work package's provider misbehaved. What the real provider owes the seam is judged by the
//! conformance suite in `plugins/rollups`.

#![allow(dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::AgentsHandle;
use bough_plugin_drift_watch::{DriftConfig, DriftHandle, DriftInner};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Cite, Class, ClassRule, HashScope, LedgerHandle, NewRollup, Order,
    Ref, Rollup, RollupId, RollupKind, RollupQuery, RowHash, Seq, Step, StepQuery, StepType,
    StepTypeDef, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_rollups::{
    DigestReport, DigestRequest, RollupsError, RollupsHandle, SealPlan, SealReport, SealRequest,
    Summarizer, SupersedeReport, SupersedeRequest,
};
use chrono::{DateTime, TimeZone, Utc};

/// The step type the reconsolidation row owns (WP-3). Declared here so the fake can append the
/// expiry note a supersession owes §3 without depending on that crate.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct MemoryExpired {
    pub targets: Vec<String>,
    pub reason: String,
    pub kind: String,
}

pub const MEMORY_EXPIRED: &str = "memory/expired";

pub fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0)
        .single()
        .expect("a fixed instant")
}

pub fn agent() -> AgentName {
    AgentName::new("scout")
}

pub fn traj() -> TrajId {
    TrajId::new("t1")
}

pub fn cfg() -> DriftConfig {
    DriftConfig {
        window_steps: 500,
        min_samples: 4,
        thought_len_cv_flag: 1.2,
        tool_entropy_flag: 0.35,
        claim_rejection_flag: 0.5,
        claim_rejection_min_decided: 4,
        max_evidence_cites: 24,
        max_state_chars: 400,
    }
}

/// `bough_plugin_drift_watch::invariant`'s record is process-global, and `harness()` clears it.
/// Every `#[tokio::test]` in a binary runs concurrently, so without this a second harness could
/// empty the record between the reset that produced an observation and the assertion that reads
/// it — leaving `evaluate(&[])`, which is trivially `Ok`, with nothing to say it happened.
static INVARIANT_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub struct Harness {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub drift: DriftHandle,
    pub summarizer: Arc<FakeSummarizer>,
    /// Held for the test's whole life: see [`INVARIANT_LOCK`].
    _guard: tokio::sync::MutexGuard<'static, ()>,
}

/// A store with every step type this phase's reset touches, and a drift handle over it.
pub async fn harness() -> Harness {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
    for def in step_types() {
        // The token is dropped, not spent: a registration is undone by an EFFECT, never by a
        // `Drop` (§0.2), so dropping it leaves the type registered for the test's life.
        drop(
            ledger
                .0
                .register_step_type(def)
                .expect("a fresh step type registers"),
        );
    }
    let summarizer = Arc::new(FakeSummarizer::new(ledger.clone()));
    let drift = DriftHandle(Arc::new(DriftInner {
        ctx: ctx.clone(),
        cfg: Arc::new(cfg()),
        ledger: ledger.clone(),
        agents: AgentsHandle::new(ctx.clone(), ledger.clone()),
        rollups: RollupsHandle(summarizer.clone()),
    }));
    let guard = INVARIANT_LOCK.lock().await;
    bough_plugin_drift_watch::invariant::reset();
    Harness {
        ctx,
        ledger,
        drift,
        summarizer,
        _guard: guard,
    }
}

fn step_types() -> Vec<StepTypeDef> {
    let mut defs = bough_plugin_about_line::step_types();
    defs.extend(bough_plugin_drift_watch::vocabulary::step_types());
    defs.push(
        StepTypeDef::of::<serde_json::Value>("thought/text", "agents")
            .class_rule(ClassRule::Thought),
    );
    defs.push(
        StepTypeDef::of::<serde_json::Value>("tool/call", "tools").class_rule(ClassRule::Thought),
    );
    defs.push(
        StepTypeDef::of::<MemoryExpired>(MEMORY_EXPIRED, "reconsolidation")
            .class_rule(ClassRule::Evidence),
    );
    defs
}

/// The agent's row, four thoughts and four calls over two tools.
pub async fn seed_trajectory(h: &Harness) {
    h.ledger
        .0
        .put_agent(AgentRow {
            name: agent(),
            traj: traj(),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("the agent row writes");

    for (kind, body) in [
        (
            "thought/text",
            serde_json::json!({ "text": "look at the ledger" }),
        ),
        ("tool/call", serde_json::json!({ "name": "bash" })),
        (
            "thought/text",
            serde_json::json!({ "text": "that failed, try again" }),
        ),
        ("tool/call", serde_json::json!({ "name": "bash" })),
        (
            "thought/text",
            serde_json::json!({ "text": "read the file instead" }),
        ),
        ("tool/call", serde_json::json!({ "name": "read" })),
        (
            "thought/text",
            serde_json::json!({ "text": "now it makes sense" }),
        ),
        ("tool/call", serde_json::json!({ "name": "bash" })),
    ] {
        append(h, kind, body, Vec::new()).await;
    }
}

pub async fn append(h: &Harness, kind: &str, body: serde_json::Value, cites: Vec<Cite>) -> Step {
    let class = if cites.is_empty() {
        Class::Thought
    } else {
        Class::Evidence
    };
    h.ledger
        .0
        .append(Append {
            traj: traj(),
            wake: WakeId::new("w1"),
            kind: StepType::new(kind),
            class,
            body,
            cites,
            at: at(),
            id: None,
        })
        .await
        .expect("the step appends")
}

/// Seal one tier block over `from..=to`. The id is namespaced `tier:` so the fake's supersession
/// can tell its own blocks from a foreign one.
pub async fn seal_tier(h: &Harness, from: u64, to: u64) -> Rollup {
    h.ledger
        .0
        .seal_rollup(NewRollup {
            id: Some(RollupId::new(format!("tier:1:{from}-{to}"))),
            traj: traj(),
            kind: RollupKind::Tier,
            tier: 1,
            from_seq: Seq(from),
            to_seq: Seq(to),
            src_trajs: vec![traj()],
            body: serde_json::json!({ "text": format!("steps {from}..{to}"), "tier": 1 }),
            notable_refs: Default::default(),
            prompt_ver: "fake-1".to_string(),
            sealed_at: at(),
        })
        .await
        .expect("the tier seals")
}

pub async fn all_steps(h: &Harness) -> Vec<Step> {
    h.ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the steps read")
}

pub async fn steps_of_kind(h: &Harness, kind: &str) -> Vec<Step> {
    h.ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            kinds: vec![StepType::new(kind)],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the steps read")
}

pub async fn tiers(h: &Harness) -> Vec<Rollup> {
    h.ledger
        .0
        .rollups(&RollupQuery {
            trajs: vec![traj()],
            kind: Some(RollupKind::Tier),
            include_superseded: true,
            ..Default::default()
        })
        .await
        .expect("the rollups read")
}

pub async fn hashes(h: &Harness, scope: HashScope) -> Vec<(String, String, Option<String>)> {
    let mut out: Vec<(String, String, Option<String>)> = h
        .ledger
        .0
        .row_hashes(scope)
        .await
        .expect("the row hashes read")
        .into_iter()
        .map(|r: RowHash| (r.id, r.hash, r.superseded_by))
        .collect();
    out.sort();
    out
}

/// A `Summarizer` that honours the seam's contract and calls no model.
pub struct FakeSummarizer {
    ledger: LedgerHandle,
    gen: AtomicU64,
    /// Every `from_raw` this fake was asked for, in call order.
    pub from_raw_seen: parking_lot::Mutex<Vec<bool>>,
    /// The raw step ids the last `rebuild_digest` read.
    pub digest_evidence: parking_lot::Mutex<Vec<String>>,
    /// Set: refuse every supersession as `NotOurs`, whatever the id.
    pub refuse_supersede: parking_lot::Mutex<bool>,
}

impl FakeSummarizer {
    pub fn new(ledger: LedgerHandle) -> Self {
        FakeSummarizer {
            ledger,
            gen: AtomicU64::new(0),
            from_raw_seen: parking_lot::Mutex::new(Vec::new()),
            digest_evidence: parking_lot::Mutex::new(Vec::new()),
            refuse_supersede: parking_lot::Mutex::new(false),
        }
    }
}

#[async_trait::async_trait]
impl Summarizer for FakeSummarizer {
    fn provider(&self) -> &'static str {
        "fake-summarizer"
    }
    fn prompt_ver(&self) -> &str {
        "fake-1"
    }

    async fn plan(&self, _req: &SealRequest) -> Result<SealPlan, RollupsError> {
        Err(RollupsError::Refused("the fake plans nothing".to_string()))
    }

    async fn seal(&self, _req: &SealRequest) -> Result<SealReport, RollupsError> {
        Err(RollupsError::Refused("the fake seals no tier".to_string()))
    }

    /// §3's relief valve: generation n+1 over the SAME range, `superseded_by` on n, an expiry
    /// note. It never rewrites the old body.
    async fn supersede(&self, req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError> {
        if *self.refuse_supersede.lock() || !req.block.as_str().starts_with("tier:") {
            return Err(RollupsError::NotOurs(req.block.clone()));
        }
        let old = self
            .ledger
            .0
            .rollups(&RollupQuery {
                include_superseded: true,
                ..Default::default()
            })
            .await?
            .into_iter()
            .find(|r| r.id == req.block)
            .ok_or_else(|| RollupsError::NotFound(req.block.clone()))?;
        if let Some(by) = &old.superseded_by {
            return Err(RollupsError::AlreadySuperseded(old.id.clone(), by.clone()));
        }
        let n = self.gen.fetch_add(1, Ordering::Relaxed) + 1;
        let new = self
            .ledger
            .0
            .seal_rollup(NewRollup {
                id: Some(RollupId::new(format!("{}#g{n}", req.block))),
                traj: old.traj.clone(),
                kind: old.kind,
                tier: old.tier,
                from_seq: old.from_seq,
                to_seq: old.to_seq,
                src_trajs: old.src_trajs.clone(),
                body: serde_json::json!({
                    "text": format!("resealed: {}", req.reason),
                    "tier": old.tier,
                    "replaces": old.id.to_string(),
                }),
                notable_refs: old.notable_refs.clone(),
                prompt_ver: self.prompt_ver().to_string(),
                sealed_at: req.at,
            })
            .await?;
        self.ledger.0.supersede_rollup(&old.id, &new.id).await?;
        let note = self
            .ledger
            .0
            .append(Append {
                traj: old.traj.clone(),
                wake: WakeId::new(format!("supersede:{}", new.id)),
                kind: StepType::new(MEMORY_EXPIRED),
                class: Class::Evidence,
                body: serde_json::to_value(MemoryExpired {
                    targets: vec![format!("rollup:{}", old.id)],
                    reason: req.reason.clone(),
                    kind: "supersession".to_string(),
                })
                .expect("MemoryExpired serialises"),
                cites: vec![Cite {
                    r#ref: Ref::rollup(&old.id),
                    url: None,
                }],
                at: req.at,
                id: None,
            })
            .await?;
        Ok(SupersedeReport {
            old: old.id,
            new: new.id,
            note: note.id,
        })
    }

    /// Reads RAW steps and seals a `digest`. It deliberately does NOT repoint
    /// `agents.digest_rollup`: the reset checks rather than trusts, and that check is what
    /// `reset_repoints_the_agent_row_at_the_new_digest` is about.
    async fn rebuild_digest(&self, req: &DigestRequest) -> Result<DigestReport, RollupsError> {
        self.from_raw_seen.lock().push(req.from_raw);
        let raw: Vec<Step> = self
            .ledger
            .0
            .steps(&StepQuery {
                trajs: vec![req.traj.clone()],
                kinds: vec![StepType::new("thought/text"), StepType::new("tool/call")],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await?;
        let evidence: Vec<String> = raw.iter().map(|s| s.id.to_string()).collect();
        *self.digest_evidence.lock() = evidence.clone();

        let tiers_read = self
            .ledger
            .0
            .rollups(&RollupQuery {
                trajs: vec![req.traj.clone()],
                kind: Some(RollupKind::Tier),
                include_superseded: true,
                ..Default::default()
            })
            .await?
            .len();

        let replaced = self
            .ledger
            .0
            .agent(&req.agent)
            .await?
            .and_then(|a| a.digest_rollup);

        let n = self.gen.fetch_add(1, Ordering::Relaxed) + 1;
        let digest = self
            .ledger
            .0
            .seal_rollup(NewRollup {
                id: Some(RollupId::new(format!("digest:{}:{n}", req.agent))),
                traj: req.traj.clone(),
                kind: RollupKind::Digest,
                tier: 0,
                from_seq: raw.first().map(|s| s.seq).unwrap_or(Seq(1)),
                to_seq: raw.last().map(|s| s.seq).unwrap_or(Seq(1)),
                src_trajs: vec![req.traj.clone()],
                body: serde_json::json!({
                    "text": format!("{} raw steps", raw.len()),
                    "evidence": evidence,
                    "from_raw": req.from_raw,
                }),
                notable_refs: Default::default(),
                prompt_ver: self.prompt_ver().to_string(),
                sealed_at: req.at,
            })
            .await?;
        if let Some(old) = &replaced {
            self.ledger.0.supersede_rollup(old, &digest.id).await?;
        }
        Ok(DigestReport {
            digest: digest.id,
            replaced,
            tiers_read,
            calls: 0,
        })
    }
}
