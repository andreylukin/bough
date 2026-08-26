//! §2's lifecycle rules, each as a named test: the first cancel cause wins, a cancel with nothing
//! active arms nothing, `Disposed` never latches a pending wake, a failed `setup` rolls the whole
//! creation back, and teardown runs in exactly one order. (V8.)

mod common;

use std::sync::Arc;

use bough_plugin_agents::{Agent, AgentError, AgentSetup, CancelCause, CreateAgent, Status};
use common::*;

/// Two cancels race; §2 says the FIRST cause wins and the driver hears exactly one.
///
/// A REAL race: `tokio::join!` polls two futures in order on one task, so the first runs to
/// completion before the second is ever polled — sequential, and no evidence about first-wins at
/// all. Two spawned tasks on a multi-threaded runtime are the honest shape.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_first_cancel_cause_wins() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    let driver = f.factory.last();
    driver.run().await;

    let a1 = agent.clone();
    let a2 = agent.clone();
    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let b1 = barrier.clone();
    let b2 = barrier.clone();
    let t1 = tokio::spawn(async move {
        b1.wait().await;
        a1.cancel(CancelCause::User, false).await
    });
    let t2 = tokio::spawn(async move {
        b2.wait().await;
        a2.cancel(CancelCause::Hook, true).await
    });
    t1.await.expect("the task joins");
    t2.await.expect("the task joins");

    let won = agent.cancelled_by().expect("a cause won");
    assert!(
        won == CancelCause::User || won == CancelCause::Hook,
        "one of the two racing causes must win, got {won:?}"
    );
    assert_eq!(
        driver.cancels().len(),
        1,
        "the driver hears the winner once and only once: {:?}",
        driver.calls()
    );
    // A later cause never displaces the winner.
    agent.cancel(CancelCause::Parent, false).await;
    assert_eq!(agent.cancelled_by(), Some(won));
    assert_eq!(driver.cancels().len(), 1);
}

/// §2: "nothing active ⇒ a no-op that never arms later work."
#[tokio::test]
async fn a_cancel_with_nothing_active_is_a_no_op_and_arms_nothing() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    let driver = f.factory.last();
    assert_eq!(agent.status(), Status::Idle);

    agent.cancel(CancelCause::User, false).await;

    assert_eq!(agent.cancelled_by(), None, "an idle cancel records nothing");
    assert!(driver.cancels().is_empty(), "the driver is not disturbed");
    assert!(
        !agent.cancel_token().is_cancelled(),
        "the token an idle cancel never fires is the token the NEXT wake uses"
    );
    assert!(!agent.has_pending_wake());
    // And the next wake runs normally: nothing was armed against it.
    driver.run().await;
    assert_eq!(agent.status(), Status::Running);
    assert_eq!(agent.cancelled_by(), None);
}

/// §2: a `Disposed` cancel never latches a pending wake.
#[tokio::test]
async fn a_disposed_cancel_never_latches_a_pending_wake() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    let driver = f.factory.last();

    agent.followup(msg("wake up")).await.expect("mail lands");
    assert!(agent.has_pending_wake(), "a followup arms a wake");
    assert_eq!(driver.notifies().len(), 1);

    agent.cancel(CancelCause::Disposed, false).await;

    assert!(agent.is_disposed());
    assert!(
        !agent.has_pending_wake(),
        "disposal un-latches the pending wake instead of leaving it armed"
    );
    // And nothing can arm another one afterwards.
    let err = agent
        .followup(msg("too late"))
        .await
        .expect_err("a disposed agent takes no mail");
    assert!(matches!(err, AgentError::Disposed { .. }), "{err}");
    assert_eq!(
        driver.notifies().len(),
        1,
        "no second notify: {:?}",
        driver.calls()
    );
    agent.when_idle().await;
}

/// The creation transaction: `setup` fails ⇒ no session, no registry entry, no scope, and not one
/// step beyond what was already durable.
#[tokio::test]
async fn a_setup_failure_rolls_the_creation_back_fully() {
    let f = Fixture::mounted().await;
    let before = f
        .ledger
        .0
        .steps(&Default::default())
        .await
        .expect("a read")
        .len();

    struct Failing {
        unwound: Arc<parking_lot::Mutex<bool>>,
    }
    #[async_trait::async_trait]
    impl AgentSetup for Failing {
        async fn setup(&self, agent: &Agent) -> Result<(), AgentError> {
            // Register something through the agent's SCOPE, so the rollback is observable.
            let flag = self.unwound.clone();
            agent
                .ctx()
                .effect(move |e| async move {
                    e.defer_sync(move || *flag.lock() = true);
                    Ok(())
                })
                .await
                .expect("a scoped registration");
            Err(AgentError::NoSuchAgent(agent.name().clone()))
        }
    }

    let unwound = Arc::new(parking_lot::Mutex::new(false));
    let mut req = CreateAgent::resident(name("sol"), f.traj(), now());
    req.setup = Some(Arc::new(Failing {
        unwound: unwound.clone(),
    }));
    req.seed = vec![(msg("seed"), bough_plugin_agents::Target::NextWake)];

    let err = f.agents.create(req).await.expect_err("setup failed");
    assert!(matches!(err, AgentError::SetupFailed { .. }), "{err}");

    assert!(f.agents.list().is_empty(), "no registry entry survives");
    assert!(f.agents.by_name(&name("sol")).is_none());
    assert!(
        f.ledger
            .0
            .agent(&name("sol"))
            .await
            .expect("a read")
            .is_none(),
        "no agent row: the durable half runs only past `setup`"
    );
    let after = f
        .ledger
        .0
        .steps(&Default::default())
        .await
        .expect("a read")
        .len();
    assert_eq!(
        after, before,
        "not one step beyond what was already durable"
    );
    assert!(*unwound.lock(), "the agent's scope unwound");
    assert!(
        f.factory.attached.lock().is_empty(),
        "the factory never sees an agent whose setup failed"
    );
}

/// §2 fixes teardown at stop+drain → unwind scope → detach agent → detach session. The driver and
/// a scope inverse record from OUTSIDE the seam, so the seam's own labels cannot be a lie.
#[tokio::test]
async fn teardown_order_is_stop_then_scope_then_agent_then_session() {
    let f = Fixture::mounted().await;
    let (agent, disposer) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    let id = agent.id().clone();
    bough_plugin_agents::trace::forget(&id);

    let scope_id = id.clone();
    agent
        .ctx()
        .effect(move |e| async move {
            e.defer_sync(move || bough_plugin_agents::trace::push(&scope_id, "scope.inverse"));
            Ok(())
        })
        .await
        .expect("a scoped registration");

    disposer.dispose().await;

    assert_eq!(
        bough_plugin_agents::trace::seen(&id),
        vec![
            "driver.stop".to_string(),
            "stop".to_string(),
            "scope.inverse".to_string(),
            "scope".to_string(),
            "agent".to_string(),
            "session".to_string(),
        ]
    );
    assert!(agent.is_disposed());
    assert!(f.agents.get(&id).is_none(), "the registry is clean");
    assert!(agent.driver().is_none(), "the session is detached");
    bough_plugin_agents::trace::forget(&id);
}

/// The disposer is a CAPABILITY: nothing else tears an agent down, and dropping it un-run leaves
/// the agent exactly as live as it was.
#[tokio::test]
async fn the_disposer_is_the_only_path_to_teardown() {
    let f = Fixture::mounted().await;
    let (agent, disposer) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    let id = agent.id().clone();

    drop(disposer);
    assert!(
        !agent.is_disposed(),
        "dropping the capability disposes nothing"
    );
    assert!(f.agents.get(&id).is_some(), "the agent is still registered");
    assert!(agent.driver().is_some(), "the session is still attached");
    // And the handle itself offers no teardown: `cancel(Disposed)` stops the agent but leaves the
    // registry entry, which only the disposer removes.
    agent.cancel(CancelCause::Disposed, false).await;
    assert!(f.agents.get(&id).is_some());
}
