//! §2: at most ONE agent factory, and the slot is held by an EFFECT — so unloading the driver row
//! frees it and another loop Provider can take it without a recompile. That is what makes the
//! phase's swap test possible.

mod common;

use std::sync::Arc;

use bough_plugin_agents::{AgentError, AgentFactory, CreateAgent};
use common::*;

/// The second taker is refused, and the refusal NAMES the driver that holds the slot.
#[tokio::test]
async fn set_factory_twice_is_an_error_naming_the_first_driver() {
    let f = fixture();
    f.agents
        .set_factory(&f.ctx, f.factory.clone() as Arc<dyn AgentFactory>)
        .await
        .expect("the slot is free");

    let err = f
        .agents
        .set_factory(&f.ctx, Arc::new(OtherFactory) as Arc<dyn AgentFactory>)
        .await;
    let err = match err {
        Ok(_) => panic!("the slot is taken; a second take must be refused"),
        Err(e) => e,
    };
    match err {
        AgentError::FactoryAlreadySet(driver) => assert_eq!(driver, "recording-loop"),
        other => panic!("wrong refusal: {other}"),
    }
    assert_eq!(
        f.agents.factory().expect("still held").driver(),
        "recording-loop",
        "a refused take leaves the incumbent in place"
    );
}

/// The swap: disposing the effect the first driver holds frees the slot for the second.
#[tokio::test]
async fn unloading_the_driver_row_frees_the_slot() {
    let f = fixture();
    let held = f
        .agents
        .set_factory(&f.ctx, f.factory.clone() as Arc<dyn AgentFactory>)
        .await
        .expect("the slot is free");
    assert_eq!(f.agents.factory().expect("held").driver(), "recording-loop");

    held.dispose().await;
    assert!(f.agents.factory().is_none(), "unloading frees the slot");

    f.agents
        .set_factory(&f.ctx, Arc::new(OtherFactory) as Arc<dyn AgentFactory>)
        .await
        .expect("the slot is free again");
    assert_eq!(f.agents.factory().expect("held").driver(), "other-loop");
}

/// With no factory the seam refuses to create rather than minting a half-live agent.
#[tokio::test]
async fn creating_without_a_factory_is_refused() {
    let f = fixture();
    let err = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect_err("no loop is mounted");
    assert!(matches!(err, AgentError::NoFactory), "{err}");
    assert!(f.agents.list().is_empty());
}

/// The registry is live: an agent is findable by id and by name until its disposer runs.
#[tokio::test]
async fn the_registry_holds_the_agent_until_its_disposer_runs() {
    let f = Fixture::mounted().await;
    let (agent, disposer) = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect("the transaction commits");
    assert_eq!(f.agents.list().len(), 1);
    assert_eq!(
        f.agents.by_name(&name("sol")).expect("by name").id(),
        agent.id()
    );
    assert!(f.agents.get(agent.id()).is_some());

    // A second agent under the same name is refused while the first is live.
    let err = f
        .agents
        .create(CreateAgent::resident(name("sol"), f.traj(), now()))
        .await
        .expect_err("already live");
    assert!(matches!(err, AgentError::AlreadyLive(_)), "{err}");

    disposer.dispose().await;
    assert!(f.agents.list().is_empty());
    bough_plugin_agents::trace::forget(agent.id());
}
