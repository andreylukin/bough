//! The SWAP mechanism, at the level of one row: every registration the `leader` row makes is an
//! effect owned by ITS fiber and scoped to the target BY SPEC. So the persona section is visible
//! to the target and to nobody else, unloading the row leaves no trace of it, and re-applying the
//! row against a different `config.agent` moves everything with it — no compile, no restart.

use crate::support;

use bough_plugin_ledger::AgentName;
use bough_plugin_projection::AssembleRequest;
use support::Fixture;

/// The leader's persona band as `agent` would read it, or `None` when it has none.
async fn leading(f: &Fixture, agent: &str) -> Option<String> {
    f.projection
        .0
        .assemble(&AssembleRequest {
            agent: AgentName::new(agent),
            wake: None,
            at: support::now(),
            budget: None,
            as_of: None,
        })
        .await
        .expect("an assembly always succeeds")
        .sections
        .into_iter()
        .find(|s| s.title == bough_plugin_leader::persona::TITLE)
        .map(|s| s.body)
}

#[tokio::test]
async fn the_persona_section_is_visible_to_the_target_only() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    f.lane("terra", &[]).await;
    f.mount_leader("sol").await;

    assert_eq!(
        leading(&f, "sol").await.as_deref(),
        Some("You are sol, and you lead."),
        "the target reads the leader's persona"
    );
    assert_eq!(
        leading(&f, "terra").await,
        None,
        "an ordinary lane is told nothing about leading"
    );
}

#[tokio::test]
async fn unloading_the_row_removes_the_section() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    f.mount_leader("sol").await;
    assert!(leading(&f, "sol").await.is_some());

    // Unloading the row is unwinding its fiber: every effect it registered runs its inverse.
    f.root.core().unwind_fiber(f.row.fiber_uid()).await;
    assert_eq!(
        leading(&f, "sol").await,
        None,
        "unload leaves no trace (§0.2)"
    );
}

#[tokio::test]
async fn moving_the_target_moves_the_section() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    f.lane("terra", &[]).await;
    f.mount_leader("sol").await;
    assert!(leading(&f, "sol").await.is_some());

    // A material config diff reloads the row: the old life unwinds, a new one applies. That is
    // the whole of the SWAP, and neither half is a recompile.
    f.root.core().unwind_fiber(f.row.fiber_uid()).await;
    let next = f.fresh_row();
    f.mount_leader_on(&next, "terra").await;

    assert_eq!(
        leading(&f, "sol").await,
        None,
        "the old target loses the section"
    );
    assert_eq!(
        leading(&f, "terra").await.as_deref(),
        Some("You are terra, and you lead."),
        "and the new one gains it"
    );
}

#[tokio::test]
async fn the_unsorted_sink_names_the_target() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    let leader = f.mount_leader("sol").await;
    assert_eq!(leader.target(), &AgentName::new("sol"));

    let sink = f.mail.sink().expect("the row installed a sink");
    assert_eq!(
        sink.agent(),
        AgentName::new("sol"),
        "the sink names the target, so moving the set moves the sink"
    );

    // And it works as a sink: a zero-match envelope reaches the leader as ordinary mail.
    let report = f
        .mail
        .route(support::envelope("nobody's mail", &["topic:orphan"]))
        .await
        .expect("routing succeeds with no recipients");
    assert!(report.matched.is_empty());
    assert!(report.unsorted.is_some(), "it lands on the queue");
    assert!(report.adopted, "and the mounted sink took it");
}

#[tokio::test]
async fn unloading_the_row_restores_the_null_sink() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    f.mount_leader("sol").await;
    assert!(f.mail.sink().is_some());

    f.root.core().unwind_fiber(f.row.fiber_uid()).await;
    assert!(
        f.mail.sink().is_none(),
        "a leaderless tree is the default again (P5-D4)"
    );

    // A leaderless tree still QUEUES rather than dropping or refusing.
    let report = f
        .mail
        .route(support::envelope("nobody's mail", &["topic:orphan"]))
        .await
        .expect("a leaderless route still succeeds");
    assert!(report.unsorted.is_some());
    assert!(!report.adopted, "nobody took it, and it waits on the queue");
}
