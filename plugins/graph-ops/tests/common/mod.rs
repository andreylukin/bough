// ---- the fixture --------------------------------------------------------------------------
//
// Offline by construction: `ledger-memory` for the store, a recording stand-in for the rollups
// seam, and a recording stand-in for `ctx.mail.ask_leader`. Neither stand-in is a shortcut around
// a seam: graph-ops writes NO rollup and NO delivery itself, so what these record is exactly the
// set of calls it is allowed to make.

#![allow(dead_code)]

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_graph_ops::{GraphConfig, GraphInner, LeaderAsk};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Cite, Class, LedgerHandle, NewRollup, Order, Ref, Rollup,
    RollupId, RollupKind, RollupQuery, Seq, Step, StepQuery, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_mail_router::Question;
use bough_plugin_rollups::{
    DigestReport, DigestRequest, RollupsError, RollupsHandle, SealPlan, SealReport, SealRequest,
    Summarizer, SupersedeReport, SupersedeRequest,
};
use chrono::{DateTime, TimeZone, Utc};
use parking_lot::Mutex;

pub fn base() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 4, 1, 9, 0, 0).unwrap()
}

pub fn traj(name: &str) -> TrajId {
    TrajId::new(format!("lane/{name}"))
}

pub fn refs(v: &[&str]) -> BTreeSet<Ref> {
    v.iter().map(|s| Ref::new(*s)).collect()
}

/// The digest seam, recorded. It seals a REAL rollup through the ledger — the id namespace and
/// the kind are the summarizer's rule (P5-D13) — so the tests can assert on stored rows rather
/// than on a mock's memory alone.
pub struct RecordingDigests {
    pub ledger: LedgerHandle,
    pub calls: Mutex<Vec<DigestRequest>>,
}

impl RecordingDigests {
    pub fn calls(&self) -> Vec<DigestRequest> {
        self.calls.lock().clone()
    }
}

#[async_trait::async_trait]
impl Summarizer for RecordingDigests {
    fn provider(&self) -> &'static str {
        "recording-digests"
    }
    fn prompt_ver(&self) -> &str {
        "test"
    }
    async fn plan(&self, _req: &SealRequest) -> Result<SealPlan, RollupsError> {
        unreachable!("a graph op never seals tiers")
    }
    async fn seal(&self, _req: &SealRequest) -> Result<SealReport, RollupsError> {
        unreachable!("a graph op never seals tiers")
    }
    async fn supersede(&self, _req: &SupersedeRequest) -> Result<SupersedeReport, RollupsError> {
        unreachable!("a graph op never supersedes")
    }
    async fn rebuild_digest(&self, req: &DigestRequest) -> Result<DigestReport, RollupsError> {
        self.calls.lock().push(req.clone());
        let id = if req.reconcile {
            RollupId::new(format!("recon:{}", req.traj))
        } else {
            RollupId::new(format!("digest:{}:inherited", req.traj))
        };
        let kind = if req.reconcile {
            RollupKind::Reconciliation
        } else {
            RollupKind::Digest
        };
        let sealed = self
            .ledger
            .0
            .seal_rollup(NewRollup {
                id: Some(id),
                traj: req.traj.clone(),
                kind,
                tier: 0,
                from_seq: Seq(1),
                to_seq: Seq(1),
                src_trajs: req.parents.clone(),
                body: serde_json::json!({ "text": "what this lane inherits" }),
                notable_refs: Default::default(),
                prompt_ver: "test".into(),
                sealed_at: req.at,
            })
            .await
            .map_err(RollupsError::from)?;
        Ok(DigestReport {
            digest: sealed.id,
            replaced: None,
            tiers_read: 0,
            calls: 1,
        })
    }
}

/// `ctx.mail.ask_leader`, recorded.
#[derive(Default)]
pub struct RecordingAsk {
    pub asked: Mutex<Vec<Question>>,
}

impl RecordingAsk {
    pub fn asked(&self) -> Vec<Question> {
        self.asked.lock().clone()
    }
}

#[async_trait::async_trait]
impl LeaderAsk for RecordingAsk {
    async fn ask(
        &self,
        q: Question,
    ) -> Result<bough_plugin_ledger::StepId, bough_plugin_graph_ops::GraphError> {
        self.asked.lock().push(q);
        Ok(bough_plugin_ledger::StepId::new("leader/question:1"))
    }
}

/// An ask seam that FAILS. §4's rule is that ambiguity reaches Andrey; a caller told "ambiguous"
/// when the question went nowhere would have no way to learn that.
pub struct FailingAsk;

#[async_trait::async_trait]
impl LeaderAsk for FailingAsk {
    async fn ask(
        &self,
        _q: Question,
    ) -> Result<bough_plugin_ledger::StepId, bough_plugin_graph_ops::GraphError> {
        Err(bough_plugin_graph_ops::GraphError::Other(anyhow::anyhow!(
            "the mail seam is down"
        )))
    }
}

pub struct Fx {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    pub digests: Arc<RecordingDigests>,
    pub ask: Arc<RecordingAsk>,
    pub graph: GraphInner,
}

pub fn cfg() -> GraphConfig {
    GraphConfig {
        digest_on_fork: false,
        protected: vec![],
    }
}

pub fn fx() -> Fx {
    fx_with(cfg())
}

/// The same fixture with an ask seam that fails.
pub fn fx_with_failing_ask() -> Fx {
    let mut f = fx_with(cfg());
    f.graph.ask = Arc::new(FailingAsk) as Arc<dyn LeaderAsk>;
    f
}

pub fn fx_with(cfg: GraphConfig) -> Fx {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx.clone()) as Arc<_>);
    for def in bough_plugin_graph_ops::step_types() {
        ledger.0.register_step_type(def).expect("a fresh step type");
    }
    let digests = Arc::new(RecordingDigests {
        ledger: ledger.clone(),
        calls: Mutex::new(Vec::new()),
    });
    let ask = Arc::new(RecordingAsk::default());
    let graph = GraphInner {
        ctx: ctx.clone(),
        ledger: ledger.clone(),
        rollups: RollupsHandle(digests.clone() as Arc<dyn Summarizer>),
        ask: ask.clone() as Arc<dyn LeaderAsk>,
        cfg: Arc::new(cfg),
    };
    Fx {
        ctx,
        ledger,
        digests,
        ask,
        graph,
    }
}

impl Fx {
    /// A lane with a row and `wakes` closed wakes on its chain. The chain is deliberately
    /// ordinary: a `wake/start`, a pin, a `wake/end`.
    pub async fn lane(&self, name: &str, routing: &[&str]) -> AgentRow {
        let t = traj(name);
        for i in 0..2u64 {
            let wake = WakeId::new(format!("{name}/w{i}"));
            self.append(
                &t,
                &wake,
                "wake/start",
                Class::Thought,
                serde_json::json!({ "urgency": "coalesced" }),
                vec![],
            )
            .await;
            self.append(
                &t,
                &wake,
                "pin/set",
                Class::Thought,
                serde_json::json!({ "title": format!("{name} note {i}"), "text": "a fact" }),
                vec![],
            )
            .await;
            self.append(
                &t,
                &wake,
                "wake/end",
                Class::Thought,
                serde_json::json!({ "reason": "completed" }),
                vec![],
            )
            .await;
        }
        let row = AgentRow {
            name: AgentName::new(name),
            traj: t,
            routing_refs: refs(routing),
            wake_classes: ["ask".to_string()].into_iter().collect(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        };
        self.ledger
            .0
            .put_agent(row.clone())
            .await
            .expect("the row lands");
        row
    }

    pub async fn append(
        &self,
        t: &TrajId,
        wake: &WakeId,
        kind: &str,
        class: Class,
        body: serde_json::Value,
        cites: Vec<Cite>,
    ) -> Step {
        self.ledger
            .0
            .append(Append {
                traj: t.clone(),
                wake: wake.clone(),
                kind: StepType::new(kind),
                class,
                body,
                cites,
                at: base(),
                id: None,
            })
            .await
            .expect("the step lands")
    }

    /// Open a wake and leave it open: the trailing suffix P5-D7 resolves down past.
    pub async fn open_wake(&self, t: &TrajId, wake: &str) -> WakeId {
        let w = WakeId::new(wake);
        self.append(
            t,
            &w,
            "wake/start",
            Class::Thought,
            serde_json::json!({ "urgency": "immediate" }),
            vec![],
        )
        .await;
        w
    }

    pub async fn steps(&self, t: &TrajId) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                trajs: vec![t.clone()],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("readable")
    }

    pub async fn steps_of_kind(&self, kind: &str) -> Vec<Step> {
        self.ledger
            .0
            .steps(&StepQuery {
                kinds: vec![StepType::new(kind)],
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("readable")
    }

    pub async fn rollups_on(&self, t: &TrajId) -> Vec<Rollup> {
        self.ledger
            .0
            .rollups(&RollupQuery {
                trajs: vec![t.clone()],
                ..Default::default()
            })
            .await
            .expect("readable")
    }

    pub async fn row(&self, name: &str) -> Option<AgentRow> {
        self.ledger
            .0
            .agent(&AgentName::new(name))
            .await
            .expect("readable")
    }
}
