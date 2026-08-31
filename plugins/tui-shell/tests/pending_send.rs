//! MERGE (track-B note 16): a message typed before the roster is up.
//!
//! The composer is painted the moment the shell mounts; `residents` raises the agents
//! asynchronously afterwards. Measured against the release binary on a cold `$BOUGH_HOME`, about
//! one submit in three landed inside that window: no `inbox/spliced` step, no `wake/start`, and
//! the text handed back to the composer under a `no focused agent` notice. Honest, and useless —
//! the person did nothing wrong and had to press Enter again, and a script watching the SCREEN
//! could not tell the bounce apart from a delivered message (the bounced text is on screen either
//! way, which is what made track B read this as a silent drop).
//!
//! The submit now WAITS and the tick sends it. These two tests are the whole contract: it lands
//! when an agent appears, and it comes back with an error when none ever does.

use crate::common;

use bough_plugin_agents::{AgentKind, CreateAgent, Target};
use bough_plugin_ledger::{AgentName, TrajId};
use bough_plugin_tui_shell::{run, PENDING_SEND_TICKS};
use common::shell_with_agents;

const TEXT: &str = "tell the eng channel the deploy is green";

/// Raise one agent the way `residents` does — created, not focused. The shell's own tick is what
/// adopts it.
async fn raise(
    agents: &bough_plugin_agents::AgentsHandle,
    name: &str,
) -> bough_plugin_agents::Agent {
    let (agent, disposer) = agents
        .create(CreateAgent {
            name: AgentName::new(name),
            traj: TrajId::new(format!("lane/{name}")),
            kind: AgentKind::Resident,
            scope: None,
            setup: None,
            seed: Vec::new(),
            at: chrono::Utc::now(),
        })
        .await
        .expect("the agent is created");
    // The disposer belongs to the roster, not to this test.
    std::mem::forget(disposer);
    agent
}

/// One tick of the event loop, as `run::run` spells it.
async fn tick(tui: &bough_plugin_tui_shell::TuiHandle) {
    tui.adopt_default_agent().await;
    run::flush_pending_send(tui).await;
}

#[tokio::test]
async fn a_message_typed_before_the_roster_is_up_waits_and_lands_when_an_agent_appears() {
    let (_ctx, tui, agents, factory) = shell_with_agents().await;
    assert!(
        tui.agent().is_none(),
        "the boot window: the shell is up and no agent is"
    );

    run::send(&tui, TEXT).await;

    // Not dropped, and NOT bounced: the pre-merge code put it back in the composer here.
    assert_eq!(
        tui.composer_text(),
        "",
        "the message is not handed back — it is queued"
    );
    assert_eq!(
        tui.pending_send().map(|p| p.text).as_deref(),
        Some(TEXT),
        "the message is waiting for somebody to send it to"
    );

    // The roster comes up, and the very next tick sends it.
    let agent = raise(&agents, "sol").await;
    tick(&tui).await;

    assert!(tui.pending_send().is_none(), "the queue drained");
    assert_eq!(
        agent.inbox().pending(Target::NextWake).len(),
        1,
        "the message is in the agent's inbox: it reached the loop, not the floor"
    );
    assert_eq!(
        factory.last().notifies(),
        1,
        "and the driver was told about it exactly once"
    );
}

/// The other half: a tree with genuinely no agents must SAY so rather than hold a message for
/// ever. Nothing the user typed is destroyed (B3) — it comes back to the composer.
#[tokio::test]
async fn a_queued_message_comes_back_to_the_composer_when_no_agent_ever_appears() {
    let (_ctx, tui, _agents, _factory) = shell_with_agents().await;
    run::send(&tui, TEXT).await;
    assert!(tui.pending_send().is_some());

    for _ in 0..PENDING_SEND_TICKS {
        tick(&tui).await;
    }

    assert!(tui.pending_send().is_none(), "the wait is bounded");
    assert_eq!(
        tui.composer_text(),
        TEXT,
        "the draft is given back, never destroyed"
    );
    let notice = tui.notice_raw().expect("a notice says why").text;
    assert!(notice.contains("no agent came up"), "{notice}");
}

/// A SECOND submit while one is already queued is given straight back rather than replacing the
/// first: two messages, neither lost.
#[tokio::test]
async fn a_second_submit_while_one_is_queued_is_handed_back_rather_than_replacing_it() {
    let (_ctx, tui, _agents, _factory) = shell_with_agents().await;
    run::send(&tui, TEXT).await;
    run::send(&tui, "and the second thing").await;

    assert_eq!(tui.pending_send().map(|p| p.text).as_deref(), Some(TEXT));
    assert_eq!(tui.composer_text(), "and the second thing");
}
