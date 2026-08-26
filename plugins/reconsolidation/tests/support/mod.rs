//! The pieces the three integration suites share: a real `ledger-memory`, a replaying `llm`, and
//! a rollups Provider double that seals the way the real one does.
//!
//! Not a test file of its own — `mod harness;` in each suite. It is here rather than in the crate
//! because a test double must never be shippable.

#![allow(dead_code)]

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_agents::AgentsHandle;
use bough_plugin_ledger::{
    Append, Class, LedgerHandle, NewRollup, Ref, RollupKind, Seq, Step, StepQuery, StepType,
    TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_llm::{AdapterName, AdapterSpec, LlmHandle, ModelMatch};
use bough_plugin_llm_replay::{ReplayAdapter, ReplayConfig, Transcript};
use bough_plugin_reconsolidation::{ReconConfig, ReconHandle, ReconInner};
use bough_plugin_rollups::{
    DigestReport, DigestRequest, RollupsError, RollupsHandle, SealPlan, SealReport, SealRequest,
    Summarizer, SupersedeReport, SupersedeRequest,
};
use chrono::{DateTime, TimeZone, Utc};

/// A stand-in body for `tool/result`, so the suite can append the kind its config expires.
#[derive(serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProbeResult {
    pub text: String,
}

pub const TRAJ: &str = "t1";
pub const AGENT: &str = "sol";

pub fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()
}

pub fn cfg() -> ReconConfig {
    ReconConfig {
        batch_steps: 400,
        stale_after_days: 90,
        expirable_kinds: vec!["mail/delivered".into(), "tool/result".into()],
        max_contradiction_pairs: 24,
        max_calls_per_pass: 6,
        distill_max_tokens: 2048,
        judge_prompt_ver: bough_plugin_reconsolidation::prompts::RECON_1.to_string(),
    }
}

/// The rollups Provider double.
///
/// It seals a `digest` rollup and appends the `rollup/sealed` step exactly as the real provider
/// does, and it PANICS on `seal`/`supersede`: "reconsolidation never seals a tier and never
/// supersedes" is then a property the tests would blow up on rather than one an assertion has to
/// remember to check.
pub struct DigestOnly {
    pub ledger: LedgerHandle,
    pub sealed: parking_lot::Mutex<usize>,
}

#[async_trait::async_trait]
impl Summarizer for DigestOnly {
    fn provider(&self) -> &'static str {
        "test-digest-only"
    }
    fn prompt_ver(&self) -> &str {
        "test-1"
    }
    async fn plan(&self, _req: &SealRequest) -> Result<SealPlan, RollupsError> {
        unreachable!("a reconsolidation pass never plans a seal")
    }
    async fn seal(&self, _req: &SealRequest) -> Result<SealReport, RollupsError> {
        unreachable!("a reconsolidation pass never seals a tier (§8)")
    }
    async fn supersede(&self, _req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError> {
        unreachable!("a reconsolidation pass never supersedes (§8)")
    }
    async fn rebuild_digest(&self, req: &DigestRequest) -> Result<DigestReport, RollupsError> {
        let n = {
            let mut held = self.sealed.lock();
            *held += 1;
            *held
        };
        let head = self.ledger.0.head_seq(&req.traj).await?.unwrap_or(Seq(0));
        let rollup = self
            .ledger
            .0
            .seal_rollup(NewRollup {
                id: Some(bough_plugin_ledger::RollupId::new(format!("digest:{n}"))),
                traj: req.traj.clone(),
                kind: RollupKind::Digest,
                tier: 0,
                from_seq: Seq(1),
                to_seq: head,
                src_trajs: vec![],
                body: serde_json::json!({ "text": format!("distilled digest {n}") }),
                notable_refs: Default::default(),
                prompt_ver: self.prompt_ver().to_string(),
                sealed_at: req.at,
            })
            .await?;
        // The `rollup/sealed` step the real provider appends. It is EVIDENCE, so it cites the row.
        self.ledger
            .0
            .append(Append {
                traj: req.traj.clone(),
                wake: WakeId::new("recon:digest"),
                kind: StepType::new("rollup/sealed"),
                class: Class::Evidence,
                body: serde_json::json!({
                    "rollup": rollup.id.to_string(),
                    "kind": "digest",
                    "tier": 0,
                    "from_seq": rollup.from_seq.0,
                    "to_seq": rollup.to_seq.0,
                    "prompt_ver": self.prompt_ver(),
                }),
                cites: vec![bough_plugin_ledger::Cite {
                    r#ref: Ref::new(format!("rollup:{}", rollup.id)),
                    url: None,
                }],
                at: req.at,
                id: None,
            })
            .await?;
        Ok(DigestReport {
            digest: rollup.id,
            replaced: None,
            tiers_read: 0,
            calls: 1,
        })
    }
}

/// A mounted row: the handle, plus the ledger the assertions read.
pub struct Mounted {
    pub recon: ReconHandle,
    pub ledger: LedgerHandle,
    pub ctx: Context,
}

/// Everything a pass needs, wired by hand — the plugin's `apply` in miniature, with no runtime.
pub async fn mount(rounds: serde_json::Value) -> Mounted {
    mount_with_rollups(rounds, |m| {
        RollupsHandle(Arc::new(DigestOnly {
            ledger: m.ledger.clone(),
            sealed: parking_lot::Mutex::new(0),
        }))
    })
    .await
}

/// The pieces every mount shares, before the rollups provider is chosen.
pub struct Wired {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub llm: LlmHandle,
}

/// `mount`, with the rollups provider chosen by the caller: `DigestOnly` for the suites that are
/// about the PASS, and the real `rollups-summarizer` for the one that is about the seam.
pub async fn mount_with_rollups(
    rounds: serde_json::Value,
    provider: impl FnOnce(&Wired) -> RollupsHandle,
) -> Mounted {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()));
    // `tool/result` is Phase 2's, declared by `tools-baseline`. This suite never mounts that row,
    // so it declares the type itself: the batch a pass reads must contain an EXPIRABLE kind for
    // any of these tests to mean anything.
    let mut defs = bough_plugin_reconsolidation::vocabulary::step_types();
    // The real `rollups-summarizer` appends `rollup/request` when it is the mounted provider.
    // `memory/expired` is the seam's one definition, declared identically by both rows, so the
    // map refcounts it rather than refusing the second declaration.
    defs.extend(bough_plugin_rollups_summarizer::step_types());
    defs.push(
        bough_plugin_ledger::StepTypeDef::of::<ProbeResult>("tool/result", "reconsolidation-tests")
            .class_rule(bough_plugin_ledger::ClassRule::Either),
    );
    for def in defs {
        // The token is dropped, not spent: registration is undone by an EFFECT, never a `Drop`.
        drop(
            ledger
                .0
                .register_step_type(def)
                .expect("memory/expired is a fresh step type"),
        );
    }

    let replay_cfg = Arc::new(ReplayConfig {
        transcript: None,
        rounds: Some(rounds),
        strict: false,
        models: "*".into(),
        delay_ms: 0,
    });
    let transcript: Transcript = ReplayAdapter::load(&replay_cfg).expect("the inline rounds parse");
    let llm = LlmHandle::new();
    llm.adapter(
        &ctx,
        AdapterSpec {
            name: AdapterName::new("llm-replay"),
            matches: ModelMatch::Any,
            adapter: Arc::new(ReplayAdapter::new(replay_cfg, transcript)),
        },
    )
    .await
    .expect("the replay adapter registers");
    // The stand-in for `model-policy`: the row deliberately leaves `call.model` empty, so
    // SOMETHING must choose. In the tree that is the policy row, and terra is what it chooses
    // for an unattended wake (P4-D3).
    ctx.on_waterfall::<bough_plugin_llm::AgentRequest, _, _>(|mut call, next| async move {
        assert_eq!(call.facts.wake_kind, bough_plugin_llm::WakeKind::Scheduled);
        assert!(
            !call.facts.answers_andrey,
            "a governance pass must never present itself as answering Andrey"
        );
        if call.call.model.is_empty() {
            call.call.model = "claude-haiku-4-5-20251001".to_string();
        }
        next.run(call).await
    })
    .await
    .expect("the policy listener registers");

    // The `llm` key, so a provider mounted over this fixture can read it the way `apply` does.
    ctx.provide::<bough_plugin_llm::Llm>(llm.clone())
        .await
        .expect("the llm key binds");
    let wired = Wired {
        ctx: ctx.clone(),
        ledger: ledger.clone(),
        llm: llm.clone(),
    };
    let rollups = provider(&wired);
    let recon = ReconHandle(Arc::new(ReconInner {
        ctx: ctx.clone(),
        cfg: Arc::new(cfg()),
        ledger: ledger.clone(),
        llm,
        agents: AgentsHandle::new(ctx.clone(), ledger.clone()),
        rollups,
    }));
    Mounted { recon, ledger, ctx }
}

/// One evidence step carrying `refs`.
pub async fn evidence(
    l: &LedgerHandle,
    kind: &str,
    refs: &[&str],
    body: serde_json::Value,
    at: DateTime<Utc>,
) -> Step {
    l.0.append(Append {
        traj: TrajId::new(TRAJ),
        wake: WakeId::new("w1"),
        kind: StepType::new(kind),
        class: Class::Evidence,
        body,
        cites: refs
            .iter()
            .map(|r| bough_plugin_ledger::Cite {
                r#ref: Ref::new(*r),
                url: None,
            })
            .collect(),
        at,
        id: None,
    })
    .await
    .expect("the step appends")
}

/// Every step of one kind on the trajectory, oldest first.
pub async fn steps_of(l: &LedgerHandle, kind: &str) -> Vec<Step> {
    l.0.steps(&StepQuery {
        trajs: vec![TrajId::new(TRAJ)],
        kinds: vec![StepType::new(kind)],
        ..Default::default()
    })
    .await
    .expect("the query runs")
}

/// The request a suite runs.
pub fn request(at: DateTime<Utc>) -> bough_plugin_reconsolidation::PassRequest {
    bough_plugin_reconsolidation::PassRequest {
        agent: bough_plugin_ledger::AgentName::new(AGENT),
        traj: TrajId::new(TRAJ),
        at,
        since: None,
        attribution: bough_plugin_rollups::Attribution::System,
        max_calls: None,
    }
}

/// `n` identical rounds. A round is CONSUMED when it answers, so a pass judging `n` pairs needs
/// `n` of them; `strict: false` makes any request past the end an empty turn, which CLEARS.
pub fn rounds(text: &str, n: usize) -> serde_json::Value {
    serde_json::Value::Array(
        (0..n)
            .map(|_| {
                serde_json::json!({ "chunks": [
                    { "type": "text", "text": text },
                    { "type": "end", "stop": "end_turn" }
                ] })
            })
            .collect(),
    )
}

/// A transcript that CONFIRMS the first `n` pairs it is asked about.
pub fn always_contradiction(n: usize) -> serde_json::Value {
    rounds("CONTRADICTION: the two disagree about the port", n)
}

/// A transcript that CLEARS every pair.
pub fn always_clear(n: usize) -> serde_json::Value {
    rounds("CLEAR", n)
}

/// A recap-shaped answer: what the REAL summarizer's digest call needs to parse, and text no
/// judge reads as a confirmation.
pub fn recap_rounds(n: usize) -> serde_json::Value {
    rounds(
        "The agent read some tool output and recorded what it found.\n\n## Open question\nWhether \
         any of it still holds.",
        n,
    )
}
