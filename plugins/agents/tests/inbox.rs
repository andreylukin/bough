//! §2: EVERY inbox mutation is a durable `inbox/spliced` step keyed by the message id, and the
//! live inbox is a cache of that fold (P2-D8). The three presets are the documented (target, wake)
//! pairs and nothing else.

mod common;

use bough_plugin_agents::{ClaimSelector, Inbox, MailClass, Target};
use bough_plugin_ledger::vocabulary::{InboxSpliced, SpliceOp};
use bough_plugin_ledger::{Step, StepQuery};
use common::*;

async fn splices(f: &Fixture) -> Vec<(String, SpliceOp)> {
    steps(f)
        .await
        .iter()
        .map(|s| {
            let b: InboxSpliced =
                serde_json::from_value((*s.body).clone()).expect("an inbox/spliced body");
            (b.message, b.op)
        })
        .collect()
}

async fn steps(f: &Fixture) -> Vec<Step> {
    f.ledger
        .0
        .steps(&StepQuery {
            kinds: vec![bough_plugin_ledger::StepType::new("inbox/spliced")],
            ..Default::default()
        })
        .await
        .expect("a read")
}

/// Insert, claim and discard each append exactly one step, keyed by the message id.
#[tokio::test]
async fn every_mutation_appends_inbox_spliced_keyed_by_the_message_id() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f
        .agents
        .create(bough_plugin_agents::CreateAgent::resident(
            name("sol"),
            f.traj(),
            now(),
        ))
        .await
        .expect("the transaction commits");
    let cell = &f.factory.last().cell;

    let kept = msg("one");
    let dropped = msg("two");
    agent
        .send(kept.clone(), Target::NextWake, false)
        .await
        .expect("insert");
    agent
        .send(dropped.clone(), Target::NextWake, false)
        .await
        .expect("insert");

    let wake = bough_plugin_ledger::WakeId::new("w-1");
    let claimed = cell
        .claim(
            ClaimSelector {
                target: Target::NextWake,
                only: Some(vec![kept.id.clone()]),
                classes: None,
                exclude_andrey: false,
                limit: None,
            },
            wake.clone(),
            now(),
        )
        .await
        .expect("a claim");
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].message.id, kept.id);

    cell.discard(&dropped.id, wake, "no longer relevant", now())
        .await
        .expect("a discard");

    assert_eq!(
        splices(&f).await,
        vec![
            (kept.id.to_string(), SpliceOp::Insert),
            (dropped.id.to_string(), SpliceOp::Insert),
            (kept.id.to_string(), SpliceOp::Claim),
            (dropped.id.to_string(), SpliceOp::Discard),
        ]
    );
    assert!(agent.inbox().is_empty(), "both left the live queue");
}

/// The fold IS the inbox: replaying the durable splices reproduces the live cache exactly.
#[tokio::test]
async fn insert_claim_and_discard_fold_back_to_the_live_inbox() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f
        .agents
        .create(bough_plugin_agents::CreateAgent::resident(
            name("sol"),
            f.traj(),
            now(),
        ))
        .await
        .expect("the transaction commits");
    let cell = &f.factory.last().cell;

    let a = msg("alpha");
    let b = msg("beta");
    let c = msg("gamma");
    agent
        .send(a.clone(), Target::NextWake, false)
        .await
        .unwrap();
    agent
        .send(b.clone(), Target::NextStep, false)
        .await
        .unwrap();
    agent
        .send(c.clone(), Target::NextWake, false)
        .await
        .unwrap();
    cell.claim(
        ClaimSelector {
            target: Target::NextWake,
            only: Some(vec![a.id.clone()]),
            classes: None,
            exclude_andrey: false,
            limit: None,
        },
        bough_plugin_ledger::WakeId::new("w-1"),
        now(),
    )
    .await
    .unwrap();

    let folded = Inbox::rebuild(&steps(&f).await);
    let live: Vec<_> = agent
        .inbox()
        .pending(Target::NextWake)
        .into_iter()
        .map(|m| (m.id, Target::NextWake))
        .chain(
            agent
                .inbox()
                .pending(Target::NextStep)
                .into_iter()
                .map(|m| (m.id, Target::NextStep)),
        )
        .collect();
    let folded_ids: Vec<_> = folded.iter().map(|(m, t)| (m.id.clone(), *t)).collect();

    assert_eq!(folded.len(), 2, "alpha was claimed out: {folded_ids:?}");
    let mut a1 = folded_ids.clone();
    let mut a2 = live.clone();
    a1.sort_by(|x, y| x.0.as_str().cmp(y.0.as_str()));
    a2.sort_by(|x, y| x.0.as_str().cmp(y.0.as_str()));
    assert_eq!(a1, a2, "the fold and the live cache cannot disagree");

    // And the whole message survives the round trip, not just its id.
    let restored = folded
        .iter()
        .find(|(m, _)| m.id == b.id)
        .map(|(m, _)| m.clone())
        .expect("beta is in the fold");
    assert_eq!(restored.text, b.text);
    assert_eq!(restored.subject, b.subject);
    assert!(restored.is_andrey());
    assert_eq!(restored.class, MailClass::Ordinary);
}

/// §2's three presets, each pinned to its documented (target, wake) pair.
#[tokio::test]
async fn the_three_presets_map_to_the_documented_target_and_wake_pairs() {
    let f = Fixture::mounted().await;
    let (agent, _d) = f
        .agents
        .create(bough_plugin_agents::CreateAgent::resident(
            name("sol"),
            f.traj(),
            now(),
        ))
        .await
        .expect("the transaction commits");

    let followup = agent.followup(msg("f")).await.expect("followup");
    assert_eq!((followup.target, followup.wake), (Target::NextWake, true));

    let steer = agent.steer(msg("s")).await.expect("steer");
    assert_eq!((steer.target, steer.wake), (Target::NextStep, true));

    let inject = agent.inject(msg("i")).await.expect("inject");
    assert_eq!((inject.target, inject.wake), (Target::NextStep, false));

    // The durable body carries the same pair, so a fold sees what the caller asked for.
    let bodies: Vec<(bough_plugin_ledger::vocabulary::SpliceTarget, bool)> = steps(&f)
        .await
        .iter()
        .map(|s| {
            let b: InboxSpliced = serde_json::from_value((*s.body).clone()).expect("a body");
            (b.target, b.wake)
        })
        .collect();
    use bough_plugin_ledger::vocabulary::SpliceTarget as T;
    assert_eq!(
        bodies,
        vec![
            (T::NextWake, true),
            (T::NextStep, true),
            (T::NextStep, false)
        ]
    );
}
