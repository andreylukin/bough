//! V1's supporting half (WP-1): a snapshot at one `as_of` is byte-identical taken twice, and
//! ignores every row above that `as_of`.
//!
//! The projector here is a MINIATURE assembler — it renders the ledger rows at or below the
//! request's `as_of` — because that is the only way "ignores every row above it" is a claim about
//! the seam at all rather than about a stub that never read anything.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Class, LedgerHandle, LedgerStore, Order, StepQuery, StepType,
    TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_projection::{
    AssembleRequest, Assembled, FileViewRequest, Place, Position, PrefixSource, PrefixToken,
    ProjectionError, ProjectionHandle, Projector, RenderedSection, SectionCites, SectionId,
    SectionSpec, SectionToken, Slot,
};
use bough_plugin_tui_preview::{snapshot, PreviewAt};
use chrono::{DateTime, TimeZone, Utc};

fn at() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 8, 27, 9, 0, 0).unwrap()
}

struct Tail(LedgerHandle);

#[async_trait::async_trait]
impl Projector for Tail {
    fn provider(&self) -> &'static str {
        "tail"
    }
    fn section(&self, _spec: SectionSpec) -> Result<SectionToken, ProjectionError> {
        unreachable!("the preview never contributes a section")
    }
    async fn assemble(&self, req: &AssembleRequest) -> Result<Assembled, ProjectionError> {
        let steps = self
            .0
             .0
            .steps(&StepQuery {
                trajs: vec![TrajId::new("t-sol")],
                before: req.as_of.map(|s| bough_plugin_ledger::Seq(s.0 + 1)),
                order: Order::SeqAsc,
                ..Default::default()
            })
            .await
            .expect("the memory store answers");
        let body = steps
            .iter()
            .map(|s| format!("- #{} {}", s.seq.0, s.kind))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(Assembled {
            agent: req.agent.clone(),
            sections: vec![RenderedSection {
                id: SectionId::new("tail"),
                position: Position {
                    slot: Slot::Tail,
                    place: Place::Band,
                },
                title: "tail".into(),
                body,
                cites: SectionCites::default(),
                tokens: steps.len(),
                degraded: None,
            }],
            flags: BTreeSet::new(),
            tokens: steps.len(),
            budget: 100,
            cites: SectionCites::default(),
        })
    }
    async fn file_view(&self, _req: &FileViewRequest) -> Result<String, ProjectionError> {
        unreachable!("the preview never renders a file view")
    }
    fn pin_prefix(
        &self,
        _agent: AgentName,
        _prefix: Assembled,
        _source: PrefixSource,
    ) -> Result<PrefixToken, ProjectionError> {
        unreachable!("the preview pins nothing")
    }
    async fn write_file_view(
        &self,
        _req: &FileViewRequest,
        _dir: Option<&std::path::Path>,
    ) -> Result<std::path::PathBuf, ProjectionError> {
        unreachable!("the preview writes nothing")
    }
}

async fn fixture() -> (LedgerHandle, ProjectionHandle) {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx) as Arc<dyn LedgerStore>);
    ledger
        .0
        .put_agent(AgentRow {
            name: AgentName::new("sol"),
            traj: TrajId::new("t-sol"),
            routing_refs: Default::default(),
            wake_classes: Default::default(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .expect("agents is mutable config");
    let projection = ProjectionHandle(Arc::new(Tail(LedgerHandle(ledger.0.clone()))));
    (ledger, projection)
}

async fn append(ledger: &LedgerHandle, id: &str) {
    ledger
        .0
        .append(Append {
            traj: TrajId::new("t-sol"),
            wake: WakeId::new("w1"),
            kind: StepType::new("wake/start"),
            class: Class::Thought,
            body: serde_json::json!({ "urgency": "immediate" }),
            cites: Vec::new(),
            at: at(),
            id: Some(bough_plugin_ledger::StepId::new(id)),
        })
        .await
        .expect("an append");
}

#[tokio::test]
async fn two_snapshots_at_one_seq_are_byte_identical() {
    let (ledger, projection) = fixture().await;
    append(&ledger, "s1").await;
    append(&ledger, "s2").await;
    let agent = AgentName::new("sol");
    let a = snapshot(
        &projection,
        &ledger,
        &agent,
        PreviewAt::Seq(bough_plugin_ledger::Seq(2)),
        at(),
    )
    .await
    .expect("a preview");
    let b = snapshot(
        &projection,
        &ledger,
        &agent,
        PreviewAt::Seq(bough_plugin_ledger::Seq(2)),
        at(),
    )
    .await
    .expect("a preview");
    assert_eq!(a.text, b.text);
    assert_eq!(a.digest, b.digest);
}

#[tokio::test]
async fn a_snapshot_at_a_seq_ignores_every_row_above_it() {
    let (ledger, projection) = fixture().await;
    append(&ledger, "s1").await;
    let agent = AgentName::new("sol");
    let anchored = PreviewAt::Seq(bough_plugin_ledger::Seq(1));
    let before = snapshot(&projection, &ledger, &agent, anchored.clone(), at())
        .await
        .expect("a preview");
    append(&ledger, "s2").await;
    let after = snapshot(&projection, &ledger, &agent, anchored, at())
        .await
        .expect("a preview");
    assert_eq!(
        before.text, after.text,
        "a row above the anchor changed nothing"
    );
    // …and the anchor is what did it: at head, the new row IS there.
    let head = snapshot(&projection, &ledger, &agent, PreviewAt::Head, at())
        .await
        .expect("a preview");
    assert_ne!(head.text, before.text);
    assert_eq!(head.as_of, bough_plugin_ledger::Seq(2));
}
