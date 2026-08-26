//! §5's catch-up entry point at the SEAM (P3-D16). `request_wake` asks the driver whether there
//! is anything to do and does nothing else: it never appends a synthetic message, never arms a
//! wake of its own, and answers `Nothing` for an agent with nothing queued.

mod common;

use bough_plugin_agents::{CreateAgent, Status, Target, WakeCause, WakeKind, WakeRequest};
use bough_plugin_ledger::StepQuery;
use common::*;

async fn step_count(f: &Fixture) -> usize {
    f.ledger
        .0
        .steps(&StepQuery::default())
        .await
        .expect("a read")
        .len()
}

#[tokio::test]
async fn request_wake_with_nothing_queued_starts_no_wake() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    let driver = f.factory.last();
    let before = step_count(&f).await;

    let req = agent
        .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
        .await;

    assert_eq!(req, WakeRequest::Nothing, "nothing queued is nothing to do");
    assert_eq!(
        driver.calls(),
        vec![DriverCall::WakeNow],
        "the seam asked the driver exactly once and did nothing itself"
    );
    assert_eq!(
        step_count(&f).await,
        before,
        "no synthetic message: harness chatter never enters the transcript (P3-D16)"
    );
    assert_eq!(agent.status(), Status::Idle);
    assert!(!agent.has_pending_wake());
}

#[tokio::test]
async fn request_wake_with_queued_mail_starts_exactly_one() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    let driver = f.factory.last();
    // An INJECT: durable, queued, and deliberately not a wake reason of its own — so the wake
    // that follows can only be the one `request_wake` asked for.
    agent
        .send(
            msg("queued while the lid was shut"),
            Target::NextStep,
            false,
        )
        .await
        .expect("insert");
    let before = step_count(&f).await;

    let req = agent
        .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
        .await;

    match req {
        WakeRequest::Started(wake) => {
            assert_eq!(driver.wakes.lock().len(), 1, "exactly one wake, not two");
            assert_eq!(driver.wakes.lock()[0], wake);
        }
        WakeRequest::Nothing => panic!("queued mail is something to catch up on"),
    }
    assert_eq!(
        driver
            .calls()
            .iter()
            .filter(|c| **c == DriverCall::WakeNow)
            .count(),
        1
    );
    assert_eq!(
        step_count(&f).await,
        before,
        "the seam appended nothing of its own"
    );
}

/// A disposed agent is terminal (§2): nothing may arm work on it, catch-up included.
#[tokio::test]
async fn request_wake_on_a_disposed_agent_is_nothing() {
    let f = Fixture::mounted().await;
    let (agent, disposer) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    agent
        .send(msg("queued"), Target::NextWake, false)
        .await
        .expect("insert");
    disposer.dispose().await;

    assert_eq!(
        agent
            .request_wake(WakeKind::Catchup, WakeCause::CatchUp)
            .await,
        WakeRequest::Nothing
    );
}
