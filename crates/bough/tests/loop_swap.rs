//! SWAP — the phase's exit gate (§17): one row introduced in this phase is REPLACED by a patch,
//! with no recompile, and the tree stays consistent.
//!
//! Two swaps, because the phase introduced two seams that a second Provider can hold: the wake
//! flow (`agent-loop` → `agent-loop-scripted`) and the model (`llm-anthropic` → `llm-replay`).

use crate::support;

use bough_kernel::{FiberState, SerialEvent, WaterfallEvent};
use bough_plugin_agents::{AgentKind, Agents, CreateAgent, MailClass, Message, MessageId, Sender};
use bough_plugin_hello::trace;
use bough_plugin_ledger::query::{Order, StepQuery};
use bough_plugin_ledger::{AgentName, Ledger, TrajId, WakeId};
use bough_plugin_tools::{ToolCall, ToolCallId, ToolName, Tools};
use bough_plugin_workers::Workers;
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

/// Boot one driver, run ONE real Andrey wake, and read back what each consumer of the swapped
/// seam actually DID. A row being `Active` is not evidence a listener still runs, which is why
/// this asserts behaviour: the model the policy chose, the about-line the wake refreshed, and a
/// baseline tool executed through the scoped registry.
/// Create the resident, send it one Andrey message and wait for it to go idle.
async fn wake_once(
    kernel: &std::sync::Arc<bough_kernel::Kernel>,
    driver: &str,
) -> (
    bough_plugin_agents::Agent,
    bough_plugin_agents::AgentDisposer,
    TrajId,
) {
    let ctx = row_ctx(kernel, "exec");
    let agents = ctx.get::<Agents>().expect("the agents key is bound");
    let traj = TrajId::new("lane/sol");
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new("sol"),
            traj: traj.clone(),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the creation transaction commits");
    agent
        .followup(Message {
            id: MessageId::new("msg-swap-consumers"),
            from: Sender::Andrey,
            class: MailClass::Wake,
            text: "say something".to_string(),
            subject: "say something".to_string(),
            cites: Vec::new(),
            refs: Default::default(),
            mail_seq: None,
            at: chrono::Utc::now(),
        })
        .await
        .expect("mail lands");
    tokio::time::timeout(std::time::Duration::from_secs(20), agent.when_idle())
        .await
        .unwrap_or_else(|_| panic!("`{driver}` never went idle"));
    (agent, disposer, traj)
}

async fn consumers_keep_working(driver: &str) {
    let (kernel, _dir) = boot_real("headless", &patches(driver)).await;
    let ctx = row_ctx(&kernel, "exec");
    let ledger = ctx.get::<Ledger>().expect("the ledger key is bound");
    let tools = row_ctx(&kernel, "tools.baseline")
        .get::<Tools>()
        .expect("the tools key is bound");
    let workers = row_ctx(&kernel, "tool.spawn_worker")
        .get::<Workers>()
        .expect("the workers key is bound");
    let name = AgentName::new("sol");
    let (_agent, disposer, traj) = wake_once(&kernel, driver).await;

    let steps = ledger
        .0
        .steps(&StepQuery {
            trajs: vec![traj.clone()],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the chain reads back");

    // model-policy — a prepend listener on `agent/request`. A wake answering Andrey must be
    // called with `sol`, and the header is where the ledger records what was actually sent.
    let header = steps
        .iter()
        .find(|s| s.kind.as_str() == "request/header")
        .unwrap_or_else(|| panic!("`{driver}` appended no request/header: {steps:#?}"));
    assert_eq!(
        header
            .body
            .get("call")
            .and_then(|c| c.get("model"))
            .and_then(|m| m.as_str()),
        Some("claude-haiku-4-5-20251001"),
        "model-policy must still choose sol for an Andrey wake under `{driver}`: {:?}",
        header.body
    );

    // about-line — a listener on `wake/end` for COMPLETED wakes. Its step must exist, cite real
    // steps (the ledger's Evidence rule) and label the intent half as intent.
    let line = bough_plugin_about_line::newest(&ledger, &traj)
        .await
        .expect("the about-line reads back")
        .unwrap_or_else(|| panic!("`{driver}` refreshed no about/line"));
    let about: bough_plugin_about_line::AboutLine =
        serde_json::from_value((*line.body).clone()).expect("an about/line body");
    assert!(
        !line.cites.is_empty(),
        "the state half cites the steps it summarises under `{driver}`"
    );
    let rendered = bough_plugin_about_line::render(&about);
    if !about.intent.trim().is_empty() {
        assert!(
            rendered.contains(bough_plugin_about_line::INTENT_LABEL),
            "the intent half stays labelled as intent under `{driver}`: {rendered}"
        );
    }

    // tools — the scoped registry and the guarded pipeline, exercised for real: the six baseline
    // tools are visible to this agent and `read_file` actually reads a file.
    let visible: Vec<String> = tools
        .visible(&name)
        .into_iter()
        .map(|t| t.as_str().to_string())
        .collect();
    for t in [
        "bash",
        "read_file",
        "write_file",
        "edit_file",
        "glob",
        "grep",
    ] {
        assert!(
            visible.contains(&t.to_string()),
            "`{t}` must stay visible to the agent under `{driver}`: {visible:?}"
        );
    }
    let results = tools
        .execute(
            &ctx,
            vec![ToolCall {
                id: ToolCallId::new("call-swap-read"),
                name: ToolName::new("read_file"),
                args: serde_json::json!({ "path": "Cargo.toml" }),
                agent: name.clone(),
                wake: WakeId::new("wake-swap-probe"),
                step_index: 0,
            }],
        )
        .await;
    assert_eq!(results.len(), 1);
    assert!(
        results[0].ok,
        "read_file must still execute under `{driver}`: {:?}",
        results[0].failure
    );
    assert!(
        results[0].content.contains("bough"),
        "the tool returned the real file under `{driver}`: {}",
        results[0].content
    );

    // workers — the seam is live with the bundle's bounds, and its tools are in the same scope.
    assert_eq!(workers.bounds().max_in_flight, 8);
    assert_eq!(workers.bounds().max_depth, 3);
    assert!(
        visible.contains(&"spawn_worker".to_string()),
        "the worker tool stays in the agent's scope under `{driver}`: {visible:?}"
    );

    disposer.dispose().await;
    kernel.shutdown().await;
}

/// The two loop Providers this gate is parameterised over.
fn patches(driver: &str) -> Vec<std::path::PathBuf> {
    match driver {
        "agent-loop" => vec![fixture("llm-replay.yml")],
        "agent-loop-scripted" => vec![fixture("loop-scripted.yml"), fixture("llm-replay.yml")],
        other => panic!("no such driver `{other}`"),
    }
}

#[tokio::test]
async fn about_line_tools_workers_and_model_policy_keep_working_against_the_live_driver() {
    let _guard = trace::test_lock();
    consumers_keep_working("agent-loop").await;
}

#[tokio::test]
async fn about_line_tools_workers_and_model_policy_keep_working_against_the_scripted_driver() {
    let _guard = trace::test_lock();
    consumers_keep_working("agent-loop-scripted").await;
}

#[tokio::test]
async fn the_ledger_and_agents_invariants_run_against_both_loop_providers() {
    let _guard = trace::test_lock();

    for driver in ["agent-loop", "agent-loop-scripted"] {
        let (kernel, _dir) = boot_real("headless", &patches(driver)).await;
        // Not a fresh boot: a tree that has never run a wake has nothing for the ledger and
        // agents invariants to be wrong about. Run a real conversation first.
        let (agent, disposer, _traj) = wake_once(&kernel, driver).await;
        kernel.run_invariants().await;
        assert!(
            kernel.violations().is_empty(),
            "a conversation under `{driver}` violates nothing: {:#?}",
            kernel.violations()
        );
        assert!(
            kernel
                .row_context(&bough_kernel::EntryId::new("ledger"))
                .is_some()
                && kernel
                    .row_context(&bough_kernel::EntryId::new("agents"))
                    .is_some(),
            "both invariant owners must be live for this gate to mean anything"
        );
        drop(agent);
        disposer.dispose().await;
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
    let uid = row(&kernel, "agent.loop")
        .uid
        .expect("the live row has a uid");
    let core = kernel.core();
    let stream_hops_before = core.listener_count(bough_plugin_llm::LlmStreamEvent::NAME);
    let counts_before: Vec<(&'static str, usize)> = [
        bough_plugin_agents::AgentPreStep::NAME,
        bough_plugin_llm::AgentRequest::NAME,
        bough_plugin_agents::AgentWakeStopping::NAME,
        bough_plugin_agents::AgentRequestError::NAME,
    ]
    .into_iter()
    .map(|e| (e, core.listener_count(e)))
    .collect();

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

    // …and the OTHER two thirds of this test's name, which used to go unchecked: no listeners and
    // no bindings are left BY THIS ROW. `agent-loop` registers no permanent listener of its own —
    // it DISPATCHES `agent/*` and installs one transient `llm/stream` hop per round — so the
    // honest statements are: the counts on those events are exactly what they were before the row
    // retired (it took nobody else's listener with it), the per-round hop is gone, and the row's
    // own bindings are gone.
    for (event, before) in counts_before {
        assert_eq!(
            core.listener_count(event),
            before,
            "retiring `agent.loop` changed the listener count on `{event}`"
        );
    }
    // NOT zero: other rows hold PERMANENT `llm/stream` hops (`model-policy`'s usage tee, and the
    // TUI's text tee). What must be gone is the loop's own per-round hop, which is exactly "the
    // count is back where it started".
    assert_eq!(
        core.listener_count(bough_plugin_llm::LlmStreamEvent::NAME),
        stream_hops_before,
        "the retired loop left a per-round `llm/stream` hop behind"
    );
    assert!(
        core.provided_by(uid).is_empty(),
        "the retired loop left a binding: {:?}",
        core.provided_by(uid)
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
