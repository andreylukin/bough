//! The stub Provider, held to its own promise: it seals NOTHING, refuses honestly, and appends no
//! step. These cases run the handle directly over a `ledger-memory` store — no kernel tree — so
//! they say something about the provider rather than about the composition (the swap gate in
//! `crates/bough/tests/rollups_swap.rs` says the composition half).

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::{Append, Class, LedgerHandle, Seq, StepQuery, StepType, TrajId, WakeId};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_rollups::{
    Attribution, DigestRequest, RollupsError, SealRequest, SkipReason, Stop, Summarizer,
};
use bough_plugin_rollups_none::NoneSummarizer;
use chrono::{TimeZone, Utc};

fn at(secs: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0)
        .single()
        .expect("a valid instant")
}

fn ledger() -> LedgerHandle {
    let ctx = Context::root(KernelCore::new());
    let handle = LedgerHandle(MemoryStore::new(ctx) as Arc<_>);
    // The seed's one step type, registered directly: this suite is about the stub, not about the
    // vocabulary some other row happens to declare.
    handle
        .0
        .register_step_type(bough_plugin_ledger::StepTypeDef::of::<serde_json::Value>(
            "thought/text",
            "rollups-none-test",
        ))
        .expect("the fixture type registers");
    handle
}

fn stub(ledger: &LedgerHandle) -> NoneSummarizer {
    NoneSummarizer {
        ledger: Arc::new(ledger.clone()),
    }
}

fn traj() -> TrajId {
    TrajId::new("lane/sol")
}

fn seal_request() -> SealRequest {
    SealRequest {
        agent: bough_plugin_ledger::AgentName::new("sol"),
        traj: traj(),
        at: at(0),
        upto: None,
        max_calls: None,
        attribution: Attribution::System,
    }
}

async fn seed(ledger: &LedgerHandle, n: usize) {
    for i in 0..n {
        ledger
            .0
            .append(Append {
                traj: traj(),
                wake: WakeId::new("wake:seed"),
                kind: StepType::new("thought/text"),
                class: Class::Thought,
                body: serde_json::json!({ "text": format!("thought {i}") }),
                cites: vec![],
                at: at(i as i64),
                id: None,
            })
            .await
            .expect("the seed appends");
    }
}

#[tokio::test]
async fn the_plan_is_total_and_every_candidate_is_refused() {
    let ledger = ledger();
    seed(&ledger, 5).await;
    let plan = stub(&ledger)
        .plan(&seal_request())
        .await
        .expect("the stub plans");
    assert!(plan.blocks.is_empty(), "the stub plans no block");
    assert_eq!(plan.head, Seq(5));
    assert_eq!(plan.upto, Seq(5));
    assert_eq!(plan.skipped.len(), 1);
    assert_eq!(plan.skipped[0].why, SkipReason::Refused);
    assert_eq!(
        (plan.skipped[0].from_seq, plan.skipped[0].to_seq),
        (Seq(1), Seq(5))
    );
}

#[tokio::test]
async fn a_seal_pass_seals_nothing_and_says_nothing_to_do() {
    let ledger = ledger();
    seed(&ledger, 5).await;
    let report = stub(&ledger)
        .seal(&seal_request())
        .await
        .expect("the stub's pass succeeds");
    assert_eq!(report.stop, Stop::NothingToDo);
    assert_eq!(report.planned, 0);
    assert!(report.sealed.is_empty());
    assert_eq!(report.calls, 0);
    assert_eq!((report.tokens_in, report.tokens_out), (0, 0));
}

#[tokio::test]
async fn a_seal_pass_appends_no_step_and_creates_no_rollup() {
    let ledger = ledger();
    seed(&ledger, 5).await;
    let before = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            ..Default::default()
        })
        .await
        .expect("the query answers")
        .len();
    stub(&ledger).seal(&seal_request()).await.expect("the pass");
    let after = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj()],
            ..Default::default()
        })
        .await
        .expect("the query answers")
        .len();
    assert_eq!(before, after, "the stub appends no step");
    let rollups = ledger
        .0
        .rollups(&bough_plugin_ledger::RollupQuery {
            trajs: vec![traj()],
            ..Default::default()
        })
        .await
        .expect("the query answers");
    assert!(rollups.is_empty(), "the stub seals no rollup");
}

#[tokio::test]
async fn supersede_and_rebuild_digest_refuse_and_say_why() {
    let ledger = ledger();
    let stub = stub(&ledger);
    let err = stub
        .supersede(&bough_plugin_rollups::SupersedeRequest {
            block: bough_plugin_ledger::RollupId::new("tier:whatever"),
            reason: "suspected bad".into(),
            at: at(0),
            attribution: Attribution::System,
        })
        .await
        .expect_err("the stub refuses");
    assert!(matches!(err, RollupsError::Refused(_)), "got {err}");

    let err = stub
        .rebuild_digest(&DigestRequest {
            agent: bough_plugin_ledger::AgentName::new("sol"),
            traj: traj(),
            at: at(0),
            attribution: Attribution::System,
            from_raw: true,
        })
        .await
        .expect_err("the stub refuses");
    assert!(matches!(err, RollupsError::Refused(_)), "got {err}");
}

#[tokio::test]
async fn an_empty_trajectory_plans_nothing_and_claims_nothing() {
    let ledger = ledger();
    let plan = stub(&ledger)
        .plan(&seal_request())
        .await
        .expect("the stub plans");
    assert_eq!(plan.head, Seq(0));
    assert!(plan.blocks.is_empty() && plan.skipped.is_empty());
}

/// The stub stamps NO `prompt_ver`, because it seals nothing to stamp (`""` iff it seals nothing).
#[test]
fn the_stub_stamps_no_prompt_ver() {
    let ledger = ledger();
    let stub = stub(&ledger);
    assert_eq!(stub.prompt_ver(), "");
    assert_eq!(stub.provider(), bough_plugin_rollups_none::PLUGIN_NAME);
}
