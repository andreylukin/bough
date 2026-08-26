//! §3/§5 and P3-D15: DELIVERED mail is a PAIR. The `mail/delivered` step is evidence and is
//! appended first; the inbox splice that follows carries its seq, so consumption — which is per
//! (agent, mail seq) — can never be scheduled against a message whose evidence is missing.

mod common;

use std::collections::BTreeSet;

use bough_plugin_agents::{Delivery, MailClass, Sender, Target};
use bough_plugin_ledger::vocabulary::MailDelivered;
use bough_plugin_ledger::{Cite, Ref, Step, StepQuery};
use common::*;

async fn steps_of(f: &Fixture, kind: &str) -> Vec<Step> {
    f.ledger
        .0
        .steps(&StepQuery {
            kinds: vec![bough_plugin_ledger::StepType::new(kind)],
            ..Default::default()
        })
        .await
        .expect("a read")
}

fn delivery(cites: Vec<Cite>) -> Delivery {
    Delivery {
        from: Sender::Collector("github".into()),
        class: MailClass::Wake,
        subject: "CI is red on main".into(),
        summary: "the delegate test failed again".into(),
        text: "the full body of the notification".into(),
        cites,
        refs: BTreeSet::from([Ref::new("gh:bough/bough#12")]),
        at: now(),
    }
}

fn cite() -> Cite {
    Cite {
        r#ref: Ref::new("gh:bough/bough#12"),
        url: Some("https://example.invalid/12".into()),
    }
}

#[tokio::test]
async fn deliver_appends_mail_delivered_then_splices_carrying_its_seq() {
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

    let receipt = agent
        .deliver(delivery(vec![cite()]))
        .await
        .expect("delivers");

    let delivered = steps_of(&f, "mail/delivered").await;
    assert_eq!(delivered.len(), 1, "exactly one evidence step");
    let body: MailDelivered =
        serde_json::from_value((*delivered[0].body).clone()).expect("a mail/delivered body");
    assert_eq!(body.subject, "CI is red on main");
    assert_eq!(body.from, Ref::new("collector:github"));
    assert_eq!(delivered[0].class, bough_plugin_ledger::Class::Evidence);

    let splices = steps_of(&f, "inbox/spliced").await;
    let insert = splices.last().expect("the splice");
    assert!(
        insert.seq > delivered[0].seq,
        "the step comes FIRST and the splice follows it: {:?} then {:?}",
        delivered[0].seq,
        insert.seq
    );
    assert_eq!(receipt.step, insert.id);

    // The spliced message carries the evidence step's seq — the half that makes it consumable.
    let queued = agent.inbox().pending(Target::NextWake);
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].mail_seq,
        Some(delivered[0].seq),
        "the splice carries the seq of the step that was appended for it"
    );
    // And the fold agrees, so a resume rebuilds the same consumable message (P2-D8).
    let all = f
        .ledger
        .0
        .steps(&StepQuery::default())
        .await
        .expect("a read");
    let rebuilt = bough_plugin_agents::Inbox::rebuild(&all);
    assert_eq!(rebuilt.len(), 1);
    assert_eq!(rebuilt[0].0.mail_seq, Some(delivered[0].seq));
    // Wake-class mail is itself a wake reason (§5).
    assert!(receipt.wake && agent.has_pending_wake());
}

#[tokio::test]
async fn delivered_mail_is_evidence_and_must_cite() {
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

    let err = agent
        .deliver(delivery(vec![]))
        .await
        .expect_err("mail that cannot say where it came from is not deliverable");
    assert!(
        err.to_string().contains("cite") || err.to_string().contains("evidence"),
        "the refusal names the missing citations: {err}"
    );
    // NEITHER half was written: no evidence step, and nothing in the inbox.
    assert!(steps_of(&f, "mail/delivered").await.is_empty());
    assert!(
        agent.inbox().is_empty(),
        "a refused delivery splices nothing"
    );
}

#[tokio::test]
async fn an_undelivered_send_still_has_no_mail_seq() {
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

    agent
        .send(msg("a plain message"), Target::NextWake, false)
        .await
        .expect("insert");

    assert!(
        steps_of(&f, "mail/delivered").await.is_empty(),
        "an ordinary send is not DELIVERED mail and writes no evidence step"
    );
    let queued = agent.inbox().pending(Target::NextWake);
    assert_eq!(queued.len(), 1);
    assert_eq!(
        queued[0].mail_seq, None,
        "consumption is defined over delivered mail only (§5)"
    );
}
