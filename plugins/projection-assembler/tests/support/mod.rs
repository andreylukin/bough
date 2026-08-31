//! The fixture the Phase-4 projection tests share: a ledger (either provider), the rollup
//! vocabulary `bough-plugin-rollups` owns, and an assembler over both.
//!
//! DEVIATION from the WP-5 file list, named on purpose: the plan lists four test files and no
//! support module. Four copies of this harness would be four places for the fixture to drift, and
//! `crates/bough/tests/support` is the precedent for putting it in one.

#![allow(dead_code)]
// The fixture builders take a row's columns as arguments; a struct per builder would be more
// ceremony than the rows have.
#![allow(clippy::too_many_arguments)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Cite, Class, ClassRule, LedgerHandle, NewRollup, Ref, Rollup,
    RollupId, RollupKind, Seq, Step, StepId, StepType, StepTypeDef, StepTypeToken, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};
use bough_plugin_projection::{AssembleRequest, Assembled, Projector};
use bough_plugin_projection_assembler::{expiry::MEMORY_EXPIRED, Assembler, AssemblerConfig};
use bough_plugin_rollups::{Beneath, TierBlock, WindowRef};
use chrono::{DateTime, TimeZone, Utc};

pub fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap()
}

pub fn traj() -> TrajId {
    TrajId::new("t-sol")
}

pub fn agent() -> AgentName {
    AgentName::new("sol")
}

/// The agent's one routing ref, so a `notable_refs` filter has something to hit and to miss.
pub const MINE: &str = "gh:bough/rebuild#1";

/// Which ledger provider a case runs against. Both must produce the same bytes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Which {
    Sqlite,
    Memory,
}

pub struct Harness {
    pub ctx: Context,
    pub ledger: LedgerHandle,
    _dir: Option<tempfile::TempDir>,
    _expiry: StepTypeToken,
}

impl Harness {
    pub fn open(which: Which) -> Harness {
        let ctx = Context::root(KernelCore::new());
        let (ledger, dir) = match which {
            Which::Memory => (LedgerHandle(MemoryStore::new(ctx.clone())), None),
            Which::Sqlite => {
                let dir = tempfile::tempdir().expect("a temp dir");
                let cfg = SqliteConfig {
                    path: dir.path().join("ledger.db"),
                    busy_timeout_ms: 5_000,
                };
                let store = SqliteStore::open(&cfg, ctx.clone()).expect("a fresh db opens");
                (LedgerHandle(store), Some(dir))
            }
        };
        let expiry = ledger
            .0
            .register_step_type(
                StepTypeDef::of::<bough_plugin_rollups::expiry::ExpiredBody>(
                    MEMORY_EXPIRED,
                    "test",
                )
                .class_rule(ClassRule::Evidence),
            )
            .expect("a fresh step type registers");
        Harness {
            ctx,
            ledger,
            _dir: dir,
            _expiry: expiry,
        }
    }

    pub async fn append(
        &self,
        id: &str,
        wake: &str,
        kind: &str,
        class: Class,
        body: serde_json::Value,
        cites: Vec<Cite>,
    ) -> StepId {
        self.ledger
            .0
            .append(Append {
                traj: traj(),
                wake: WakeId::new(wake),
                kind: StepType::new(kind),
                class,
                body,
                cites,
                at: at(),
                // Supplied so the goldens are byte-stable (P1-D6).
                id: Some(StepId::new(id)),
            })
            .await
            .unwrap_or_else(|e| panic!("append {id} ({kind}): {e}"))
            .id
    }

    /// A verbatim step with a body long enough to weigh something in the budget.
    pub async fn note(&self, id: &str, wake: &str, text: &str) -> StepId {
        self.append(
            id,
            wake,
            "claim/proposed",
            Class::Thought,
            serde_json::json!({
                "claim": id, "kind": "observation",
                "title": text, "body": text,
            }),
            Vec::new(),
        )
        .await
    }

    pub async fn pin(&self, id: &str, title: &str, text: &str) -> StepId {
        self.append(
            id,
            "w1",
            "pin/set",
            Class::Thought,
            serde_json::json!({ "title": title, "text": text, "supersedes": [] }),
            Vec::new(),
        )
        .await
    }

    pub async fn mail(&self, id: &str, class: &str, from: &str, subject: &str) -> StepId {
        self.append(
            id,
            "w1",
            "mail/delivered",
            Class::Evidence,
            serde_json::json!({
                "class": class, "from": from, "subject": subject,
                "summary": subject, "refs": [],
            }),
            vec![Cite {
                r#ref: Ref::new(from),
                url: None,
            }],
        )
        .await
    }

    /// One appended `memory/expired` marker. EVIDENCE: the ledger itself refuses an uncited one,
    /// which is §8's "never silent" enforced by the schema rather than by the projector.
    pub async fn expire(&self, id: &str, targets: &[&str], reason: &str) -> StepId {
        self.append(
            id,
            "w-gov",
            MEMORY_EXPIRED,
            Class::Evidence,
            serde_json::json!({ "targets": targets, "reason": reason, "kind": "expiry" }),
            vec![Cite {
                r#ref: Ref::new("gov:reconsolidation"),
                url: None,
            }],
        )
        .await
    }

    /// Seal a tier block with the vocabulary `bough-plugin-rollups` owns, so the fixture is the
    /// same shape a real `rollups-summarizer` pass writes.
    pub async fn tier(
        &self,
        id: &str,
        tier: u8,
        from: u64,
        to: u64,
        text: &str,
        notable: &[&str],
        beneath: Beneath,
    ) -> RollupId {
        let block = TierBlock {
            text: text.to_string(),
            themes: Vec::new(),
            beneath,
            evidence: (from..=to).map(|n| StepId::new(format!("s{n}"))).collect(),
            windows: vec![WindowRef {
                from_seq: Seq(from),
                to_seq: Seq(to),
                cut: bough_plugin_rollups::Cut::Gap,
            }],
            tier,
            prompt_ver: "r4.1".to_string(),
        };
        self.seal(
            id,
            RollupKind::Tier,
            tier,
            from,
            to,
            serde_json::to_value(&block).expect("a tier block serializes"),
            notable,
        )
        .await
    }

    pub async fn digest(&self, id: &str, from: u64, to: u64, text: &str) -> RollupId {
        let block = bough_plugin_rollups::DigestBlock {
            text: text.to_string(),
            standing: Vec::new(),
            evidence: Vec::new(),
            from_blocks: Vec::new(),
            replaces: None,
            prompt_ver: "r4.1".to_string(),
        };
        self.seal(
            id,
            RollupKind::Digest,
            0,
            from,
            to,
            serde_json::to_value(&block).expect("a digest block serializes"),
            &[],
        )
        .await
    }

    async fn seal(
        &self,
        id: &str,
        kind: RollupKind,
        tier: u8,
        from: u64,
        to: u64,
        body: serde_json::Value,
        notable: &[&str],
    ) -> RollupId {
        self.ledger
            .0
            .seal_rollup(NewRollup {
                id: Some(RollupId::new(id)),
                traj: traj(),
                kind,
                tier,
                from_seq: Seq(from),
                to_seq: Seq(to),
                src_trajs: vec![traj()],
                body,
                notable_refs: notable.iter().map(Ref::new).collect(),
                prompt_ver: "r4.1".to_string(),
                sealed_at: at(),
            })
            .await
            .unwrap_or_else(|e| panic!("seal {id}: {e}"))
            .id
    }

    pub async fn put_agent(&self, digest: Option<&str>) {
        self.ledger
            .0
            .put_agent(AgentRow {
                name: agent(),
                traj: traj(),
                routing_refs: BTreeSet::from([Ref::new(MINE)]),
                wake_classes: BTreeSet::from(["ordinary".to_string()]),
                model_override: None,
                tick_floor: None,
                digest_rollup: digest.map(RollupId::new),
            })
            .await
            .expect("agents is mutable config");
    }

    pub async fn head(&self) -> Seq {
        self.ledger
            .0
            .head_seq(&traj())
            .await
            .expect("head_seq is a read")
            .expect("the fixture appended rows")
    }

    pub async fn rollup(&self, id: &str) -> Option<Rollup> {
        self.ledger
            .0
            .rollups(&Default::default())
            .await
            .expect("a read")
            .into_iter()
            .find(|r| r.id.as_str() == id)
    }

    pub async fn steps(&self) -> Vec<Step> {
        self.ledger
            .0
            .steps(&Default::default())
            .await
            .expect("a read")
    }

    /// Assemble with this config and no `as_of`.
    pub async fn assemble(&self, cfg: AssemblerConfig) -> Assembled {
        self.assemble_at(cfg, None).await
    }

    pub async fn assemble_at(&self, cfg: AssemblerConfig, as_of: Option<Seq>) -> Assembled {
        let assembler = Assembler::new(Arc::new(cfg), self.ledger.clone(), self.ctx.clone());
        assembler
            .assemble(&AssembleRequest {
                as_of,
                agent: agent(),
                wake: None,
                at: at(),
                budget: None,
            })
            .await
            .expect("an answer wake must always be buildable")
    }
}

/// The config every case starts from; a case names the budget it is testing.
pub fn cfg(budget: usize) -> AssemblerConfig {
    AssemblerConfig {
        budget_tokens: budget,
        headroom: 1.0,
        tail_steps: 12,
        tail_floor_steps: 3,
        dialogue_steps: 0,
        mail_newest_n: 2,
        max_tiers: 3,
        file_view_dir: PathBuf::from("/unused-by-these-tests"),
    }
}

/// Which section ids survived, in order.
pub fn ids(a: &Assembled) -> Vec<String> {
    a.sections.iter().map(|s| s.id.to_string()).collect()
}

/// The body of one section, if it survived.
pub fn body<'a>(a: &'a Assembled, id: &str) -> Option<&'a str> {
    a.sections
        .iter()
        .find(|s| s.id.as_str() == id)
        .map(|s| s.body.as_str())
}
