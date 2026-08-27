//! §2's three leader powers, each of which PROPOSES or CURATES and none of which decides:
//! adoption routes an unsorted item to a lane (or holds it), drafting a requirement produces a
//! claim and never a pin, and a timeline entry is cited evidence.

mod support;

use bough_plugin_ledger::{AgentName, Order, StepQuery, StepType, TrajId};
use support::Fixture;

/// The unsorted queue's step ids, oldest first.
async fn queue(f: &Fixture) -> Vec<bough_plugin_ledger::StepId> {
    f.mail
        .unsorted(50)
        .await
        .expect("the queue reads")
        .into_iter()
        .map(|s| s.id)
        .collect()
}

/// Every step of one kind on the unsorted trajectory.
async fn steps_of(f: &Fixture, kind: &str) -> Vec<bough_plugin_ledger::Step> {
    f.ledger
        .0
        .steps(&StepQuery {
            trajs: vec![TrajId::new(support::UNSORTED)],
            kinds: vec![StepType::new(kind)],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the query runs")
}

/// One unrouted item on the queue, with no sink mounted to take it first.
async fn one_unrouted(f: &Fixture, subject: &str) -> bough_plugin_ledger::StepId {
    f.mail
        .route(support::envelope(subject, &["topic:orphan"]))
        .await
        .expect("routing succeeds with no recipients")
        .unsorted
        .expect("a zero-match envelope lands on the queue")
}

#[tokio::test]
async fn adopt_routes_an_unsorted_item_to_a_lane() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    let terra = f.lane("terra", &["topic:ground"]).await;
    // The queue is filled BEFORE the leader mounts: a tree may boot leaderless (P5-D4).
    let item = one_unrouted(&f, "who owns the ground?").await;
    let leader = f.mount_leader("sol").await;

    let before = terra.inbox().len();
    let report = leader
        .adopt(bough_plugin_leader::AdoptRequest {
            steps: None,
            placements: vec![(item.clone(), AgentName::new("terra"))],
            at: support::now(),
        })
        .await
        .expect("adoption succeeds");

    assert_eq!(report.adopted, vec![(item, AgentName::new("terra"))]);
    assert!(report.held.is_empty());
    assert_eq!(
        terra.inbox().len(),
        before + 1,
        "the item reaches the lane as ordinary mail"
    );
}

#[tokio::test]
async fn adopt_appends_mail_adopted_naming_the_unrouted_step() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    f.lane("terra", &["topic:ground"]).await;
    let item = one_unrouted(&f, "who owns the ground?").await;
    let leader = f.mount_leader("sol").await;

    leader
        .adopt(bough_plugin_leader::AdoptRequest {
            steps: None,
            placements: vec![(item.clone(), AgentName::new("terra"))],
            at: support::now(),
        })
        .await
        .expect("adoption succeeds");

    let adopted = steps_of(&f, "mail/adopted").await;
    assert_eq!(adopted.len(), 1, "one item, one adoption");
    assert_eq!(
        adopted[0].body.get("unrouted").and_then(|v| v.as_str()),
        Some(item.as_str()),
        "the adoption names the item it consumed, which is what makes it attributable"
    );
    assert_eq!(
        adopted[0].body.get("to").and_then(|v| v.as_str()),
        Some("terra")
    );

    // The invariant this crate owns reads exactly that pairing, and it is clean.
    let (obs, unrouted) =
        bough_plugin_leader::invariant::read(&f.ledger, &TrajId::new(support::UNSORTED))
            .await
            .expect("the invariant's read runs");
    bough_plugin_leader::invariant::evaluate(&obs, &unrouted)
        .expect("one adoption of one item that exists");
}

#[tokio::test]
async fn adopt_holds_what_it_cannot_place() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    f.lane("terra", &["topic:ground"]).await;
    let placeable = one_unrouted(&f, "who owns the ground?").await;
    let puzzle = one_unrouted(&f, "and what is this?").await;
    let leader = f.mount_leader("sol").await;

    let report = leader
        .adopt(bough_plugin_leader::AdoptRequest {
            steps: None,
            placements: vec![(placeable.clone(), AgentName::new("terra"))],
            at: support::now(),
        })
        .await
        .expect("adoption succeeds");

    assert_eq!(report.adopted, vec![(placeable, AgentName::new("terra"))]);
    assert_eq!(
        report.held,
        vec![puzzle.clone()],
        "an item the leader cannot place is HELD, never forced into the nearest lane"
    );
    // Holding means STAYING: the queue still has it, and nothing was appended for it.
    assert!(queue(&f).await.contains(&puzzle));
    assert_eq!(steps_of(&f, "mail/adopted").await.len(), 1);
}

#[tokio::test]
async fn draft_requirement_produces_a_claim_and_never_a_pin() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    let leader = f.mount_leader("sol").await;

    let words = bough_plugin_ledger::Cite {
        r#ref: bough_plugin_ledger::Ref::new("step:andrey-said-so"),
        url: None,
    };
    let claim = leader
        .draft_requirement(bough_plugin_leader::DraftRequest {
            traj: TrajId::new("t-sol"),
            wake: None,
            title: "the strip shows every lane".to_string(),
            body: "including the dormant ones".to_string(),
            from: vec![words],
            supersedes: vec![],
            at: support::now(),
        })
        .await
        .expect("the draft is proposed");

    assert!(matches!(
        claim.kind,
        bough_plugin_claims::ClaimKind::Requirement { .. }
    ));
    assert_eq!(claim.by, AgentName::new("sol"));

    // The claim is OPEN: nobody has decided it, and no pin exists anywhere.
    let open = f
        .claims
        .open(&bough_plugin_claims::ClaimQuery::default())
        .await
        .expect("the open list reads");
    assert!(open.iter().any(|c| c.claim == claim.claim));
    let pins = f
        .ledger
        .0
        .steps(&StepQuery {
            kinds: vec![StepType::new("pin/set")],
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await
        .expect("the query runs");
    assert!(
        pins.is_empty(),
        "acceptance is Andrey's act (§16): drafting pins nothing"
    );
}

#[tokio::test]
async fn note_timeline_appends_a_cited_entry() {
    let f = Fixture::open().await;
    f.lane("sol", &[]).await;
    f.lane("terra", &[]).await;
    let leader = f.mount_leader("sol").await;

    let about = chrono::DateTime::parse_from_rfc3339("2026-03-04T05:06:07Z")
        .expect("a fixed moment")
        .with_timezone(&chrono::Utc);
    let step = leader
        .note_timeline(bough_plugin_leader::TimelineEntry {
            title: "terra took the ground".to_string(),
            at: about,
            agents: vec![AgentName::new("terra")],
            refs: vec![bough_plugin_ledger::Ref::new("topic:ground")],
            cites: vec![bough_plugin_ledger::Cite {
                r#ref: bough_plugin_ledger::Ref::new("step:s1"),
                url: None,
            }],
        })
        .await
        .expect("the entry appends");

    let stored = f
        .ledger
        .0
        .step(&step)
        .await
        .expect("the read runs")
        .expect("the entry is durable");
    assert_eq!(
        stored.class,
        bough_plugin_ledger::Class::Evidence,
        "a timeline is rendered as truth (§16)"
    );
    assert_eq!(stored.cites.len(), 1);

    // Read back through the seam, at the moment the entry is ABOUT rather than when it was written.
    let rows = leader
        .timeline(&bough_plugin_leader::TimelineQuery {
            agent: Some(AgentName::new("terra")),
            ..Default::default()
        })
        .await
        .expect("the timeline reads");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].title, "terra took the ground");
    assert_eq!(rows[0].at, about);

    // An entry with no cites is not appendable at all: the ledger refuses Evidence without them.
    let refused = leader
        .note_timeline(bough_plugin_leader::TimelineEntry {
            title: "an assertion nobody can check".to_string(),
            at: about,
            agents: vec![],
            refs: vec![],
            cites: vec![],
        })
        .await;
    assert!(refused.is_err(), "evidence without citations is refused");
}
