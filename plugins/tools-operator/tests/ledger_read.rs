//! The two read verbs over a real in-memory ledger: a drill is paged and CITED, and `inbox` shows
//! exactly the delivered mail no `wake/end` has consumed.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::vocabulary::{MailClass, MailDelivered, PinSet, WakeEnd, WakeEndReason};
use bough_plugin_ledger::{
    AgentName, AgentRow, Append, Cite, Class, LedgerHandle, Ref, SeqRange, StepType, TrajId, WakeId,
};
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_tools_operator::ledger_read::drill;
use bough_plugin_tools_operator::OperatorConfig;

fn cfg(page: usize) -> OperatorConfig {
    OperatorConfig {
        max_view_bytes: 1_000_000,
        max_files_per_patch: 8,
        bg_log_dir: PathBuf::from("/tmp"),
        bg_max: 4,
        bg_poll_ms: 20,
        ledger_page: page,
        schedule_max_horizon_days: 30,
        schedule_tick_ms: 1_000,
        sh_max_legs: 8,
        sh_timeout_ms: 120_000,
        sh_tags_min: 3,
        sh_tags_max: 5,
    }
}

fn now() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now()
}

fn agent() -> AgentName {
    AgentName::new("lane")
}

fn traj() -> TrajId {
    TrajId::new("t-lane")
}

/// A ledger with an `agents` row, so `connected()` resolves a membership rather than degrading to
/// the rowless one.
async fn ledger() -> LedgerHandle {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx) as Arc<_>);
    ledger
        .0
        .put_agent(AgentRow {
            name: agent(),
            traj: traj(),
            routing_refs: BTreeSet::new(),
            wake_classes: BTreeSet::new(),
            model_override: None,
            tick_floor: None,
            digest_rollup: None,
        })
        .await
        .unwrap();
    ledger
}

async fn pin(ledger: &LedgerHandle, title: &str, text: &str) -> bough_plugin_ledger::StepId {
    ledger
        .0
        .append(Append {
            traj: traj(),
            wake: WakeId::new("w1"),
            kind: StepType::new("pin/set"),
            class: Class::Thought,
            body: serde_json::to_value(PinSet {
                title: title.to_string(),
                text: text.to_string(),
                supersedes: vec![],
            })
            .unwrap(),
            cites: vec![],
            at: now(),
            id: None,
        })
        .await
        .unwrap()
        .id
}

#[tokio::test]
async fn a_tail_is_paged_and_every_row_is_cited() {
    let ledger = ledger().await;
    for n in 0..8 {
        pin(&ledger, &format!("pin {n}"), "body").await;
    }
    let out = drill(
        &ledger,
        &cfg(3),
        &agent(),
        &serde_json::json!({ "op": "tail", "n": 100 }),
    )
    .await
    .expect("a tail reads");
    assert_eq!(out.cites.len(), 3, "`n` is clamped to ledger_page");
    assert_eq!(
        out.content.lines().count(),
        4,
        "one header plus one line per step: {}",
        out.content
    );
    for c in &out.cites {
        assert!(
            c.r#ref.as_str().starts_with("step:"),
            "a drill cites the steps it read: {:?}",
            c.r#ref
        );
    }
    // The cites are what make the result EVIDENCE rather than a claim about the past.
    assert!(!out.cites.is_empty());
}

#[tokio::test]
async fn a_search_finds_a_term_and_a_range_reads_between_seqs() {
    let ledger = ledger().await;
    pin(&ledger, "first", "the octopus is asleep").await;
    pin(&ledger, "second", "nothing to see").await;
    pin(&ledger, "third", "another octopus entirely").await;

    let out = drill(
        &ledger,
        &cfg(50),
        &agent(),
        &serde_json::json!({ "op": "search", "q": "octopus" }),
    )
    .await
    .unwrap();
    assert_eq!(out.cites.len(), 2, "two of the three mention it");
    assert!(out.content.contains("2 hit(s)"), "{}", out.content);

    let out = drill(
        &ledger,
        &cfg(50),
        &agent(),
        &serde_json::json!({ "op": "steps", "from": 1, "to": 3 }),
    )
    .await
    .unwrap();
    assert_eq!(
        out.cites.len(),
        1,
        "`from` is exclusive and `to` inclusive: seq 2 only"
    );

    let bad = drill(
        &ledger,
        &cfg(50),
        &agent(),
        &serde_json::json!({ "op": "sing" }),
    )
    .await
    .expect_err("an unknown op is refused");
    assert!(bad.message.contains("search|steps|tail"), "{}", bad.message);
}

#[tokio::test]
async fn the_page_bound_holds_for_search_and_steps_too() {
    let ledger = ledger().await;
    for n in 0..10 {
        pin(&ledger, &format!("pin {n}"), "octopus").await;
    }
    for op in [
        serde_json::json!({ "op": "search", "q": "octopus", "limit": 100 }),
        serde_json::json!({ "op": "steps", "limit": 100 }),
        serde_json::json!({ "op": "tail", "n": 100 }),
    ] {
        let out = drill(&ledger, &cfg(4), &agent(), &op).await.unwrap();
        assert_eq!(out.cites.len(), 4, "ledger_page bounds every op: {op}");
    }
}

// ---------------------------------------------------------------------------
// inbox
// ---------------------------------------------------------------------------

async fn deliver(ledger: &LedgerHandle, subject: &str) -> bough_plugin_ledger::Seq {
    ledger
        .0
        .append(Append {
            traj: traj(),
            wake: WakeId::new("wake:outside"),
            kind: StepType::new("mail/delivered"),
            class: Class::Evidence,
            body: serde_json::to_value(MailDelivered {
                class: MailClass::Ordinary,
                from: Ref::new("andrey"),
                subject: subject.to_string(),
                summary: "a summary".to_string(),
                refs: vec![],
            })
            .unwrap(),
            cites: vec![Cite {
                r#ref: Ref::new("andrey"),
                url: None,
            }],
            at: now(),
            id: None,
        })
        .await
        .unwrap()
        .seq
}

#[tokio::test]
async fn inbox_is_empty_once_a_wake_end_consumed_the_seqs() {
    let ledger = ledger().await;
    let a = deliver(&ledger, "first").await;
    let b = deliver(&ledger, "second").await;

    let mail = ledger.0.unconsumed_mail(&traj()).await.unwrap();
    let out = bough_plugin_tools_operator::inbox::render(&mail);
    assert!(out.content.contains("2 unconsumed"), "{}", out.content);
    assert!(out.content.contains("first") && out.content.contains("second"));
    assert_eq!(out.cites.len(), 2, "each piece of mail is cited");

    ledger
        .0
        .append(Append {
            traj: traj(),
            wake: WakeId::new("w9"),
            kind: StepType::new("wake/end"),
            class: Class::Thought,
            body: serde_json::to_value(WakeEnd {
                reason: WakeEndReason::Completed,
                cause: None,
                consumed: vec![SeqRange { from: a, to: b }],
            })
            .unwrap(),
            cites: vec![],
            at: now(),
            id: None,
        })
        .await
        .unwrap();

    let mail = ledger.0.unconsumed_mail(&traj()).await.unwrap();
    let out = bough_plugin_tools_operator::inbox::render(&mail);
    assert!(
        out.content.contains("nothing unconsumed"),
        "reading the inbox is not what consumes it — a wake/end is: {}",
        out.content
    );
}
