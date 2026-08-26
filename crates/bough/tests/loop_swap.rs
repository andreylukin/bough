//! SWAP — the phase's exit gate (§17): one row introduced in this phase is REPLACED by a patch,
//! with no recompile, and the tree stays consistent.
//!
//! Two swaps, because the phase introduced two seams that a second Provider can hold: the wake
//! flow (`agent-loop` → `agent-loop-scripted`) and the model (`llm-anthropic` → `llm-replay`).

mod support;

use bough_kernel::FiberState;
use bough_plugin_agents::Agents;
use bough_plugin_hello::trace;
use support::{boot_real, fixture, row, row_ctx};

#[tokio::test]
async fn a_patch_mounts_agent_loop_scripted_in_place_of_agent_loop_without_a_recompile() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real(
        "headless",
        &[fixture("loop-scripted.yml"), fixture("llm-replay.yml")],
    )
    .await;

    let r = row(&kernel, "agent.loop");
    assert_eq!(r.plugin.as_deref(), Some("agent-loop-scripted"));
    assert_eq!(r.state, FiberState::Active);

    // The seam, not the row: the factory slot is held, and by the scripted driver.
    let agents = row_ctx(&kernel, "exec").get::<Agents>().unwrap();
    let factory = agents
        .factory()
        .expect("a loop provider holds the factory slot");
    assert_eq!(factory.driver(), "agent-loop-scripted");

    kernel.shutdown().await;
}

#[tokio::test]
async fn about_line_tools_workers_and_model_policy_keep_working_against_the_scripted_driver() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real(
        "headless",
        &[fixture("loop-scripted.yml"), fixture("llm-replay.yml")],
    )
    .await;

    // Every consumer of the swapped seam is still ACTIVE: a swap that quietly took its listeners
    // down would look identical from the driver's side.
    for id in [
        "about.line",
        "tools",
        "tools.baseline",
        "workers",
        "worker.spawn",
        "model.policy",
        "tool.spawn_worker",
        "tool.ask",
        "actions",
        "tool.actions",
    ] {
        assert_eq!(
            row(&kernel, id).state,
            FiberState::Active,
            "row `{id}` must survive the loop swap"
        );
    }

    kernel.shutdown().await;
}

#[tokio::test]
async fn the_ledger_and_agents_invariants_run_against_both_loop_providers() {
    let _guard = trace::test_lock();

    for patches in [
        vec![fixture("llm-replay.yml")],
        vec![fixture("loop-scripted.yml"), fixture("llm-replay.yml")],
    ] {
        let (kernel, _dir) = boot_real("headless", &patches).await;
        kernel.run_invariants().await;
        assert!(
            kernel.violations().is_empty(),
            "a freshly booted tree violates nothing under {patches:?}: {:#?}",
            kernel.violations()
        );
        kernel.shutdown().await;
    }
}

#[tokio::test]
async fn the_retired_loop_leaves_no_factory_no_listeners_and_no_bindings() {
    let _guard = trace::test_lock();
    let (kernel, dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;

    let agents = row_ctx(&kernel, "exec").get::<Agents>().unwrap();
    assert_eq!(
        agents
            .factory()
            .expect("the live loop holds the slot")
            .driver(),
        "agent-loop"
    );

    // Retire the loop through the LAUNCHER'S OWN live path — a user patch layer disabling the
    // row, recomposed. The factory slot is an effect, so retiring the row must free it.
    support::write_patch(&dir, "entries:\n  agent.loop:\n    disabled: true\n");
    support::recompose(&kernel, "", &dir)
        .await
        .expect("disabling one row recomposes cleanly");
    assert_eq!(
        row(&kernel, "agent.loop").state,
        FiberState::Inactive,
        "the disabled row must be down"
    );

    assert!(
        agents.factory().is_none(),
        "the retired loop must leave the factory slot empty — that is what lets a second \
         Provider take it"
    );

    kernel.shutdown().await;
}

#[tokio::test]
async fn a_patch_replaces_llm_anthropic_with_llm_replay() {
    let _guard = trace::test_lock();
    let (kernel, _dir) = boot_real("headless", &[fixture("llm-replay.yml")]).await;

    let r = row(&kernel, "llm.anthropic");
    assert_eq!(r.plugin.as_deref(), Some("llm-replay"));
    assert_eq!(r.state, FiberState::Active);
    assert_eq!(
        row(&kernel, "llm").state,
        FiberState::Active,
        "the Definition row does not move when its Provider is swapped"
    );

    kernel.shutdown().await;
}
