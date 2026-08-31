//! Invariant: fixtures only. Nothing here is used by the loop itself — it exists so the unit
//! tests and the crate's integration tests build the SAME shapes (a step, a message) rather than
//! two drifting spellings of them.

use std::collections::BTreeSet;
use std::sync::Arc;

use bough_plugin_agents::{MailClass, Message, MessageId, Sender};
use bough_plugin_ledger::{Class, Seq, Step, StepId, StepType, TrajId, WakeId};
use chrono::{DateTime, TimeZone, Utc};

/// A fixed clock: every fixture step lands at the same instant, so a golden never moves.
pub fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(1_700_000_000 + secs, 0).unwrap()
}

pub fn wake_of(id: &str) -> WakeId {
    WakeId::new(id)
}

pub fn traj() -> TrajId {
    TrajId::new("lane/sol")
}

/// One committed step, as a provider would have returned it.
pub fn step(seq: u64, wake: &WakeId, kind: &str, body: serde_json::Value) -> Step {
    Step {
        id: StepId::new(format!("s{seq}")),
        traj: traj(),
        seq: Seq(seq),
        at: at(seq as i64),
        wake: wake.clone(),
        kind: StepType::new(kind),
        class: Class::Thought,
        body: Arc::new(body),
        cites: Arc::new(Vec::new()),
        refs: Arc::new(BTreeSet::new()),
        ignorable: false,
    }
}

/// One piece of mail from Andrey (wake class, by §5's rule).
pub fn andrey(id: &str, text: &str) -> Message {
    Message {
        id: MessageId::new(id),
        from: Sender::Andrey,
        class: MailClass::Wake,
        text: text.to_string(),
        subject: text.chars().take(40).collect(),
        cites: Vec::new(),
        refs: BTreeSet::new(),
        mail_seq: None,
        at: at(0),
    }
}

/// One piece of ordinary mail (a push, CI, a state change).
pub fn ordinary(id: &str, seq: Option<u64>) -> Message {
    Message {
        id: MessageId::new(id),
        from: Sender::Collector("github".into()),
        class: MailClass::Ordinary,
        text: format!("ordinary {id}"),
        subject: format!("ordinary {id}"),
        cites: Vec::new(),
        refs: BTreeSet::new(),
        mail_seq: seq.map(Seq),
        at: at(0),
    }
}

/// One piece of wake-class mail from another agent (an ask, a review request).
pub fn wake_class(id: &str, seq: Option<u64>) -> Message {
    Message {
        id: MessageId::new(id),
        from: Sender::Agent(bough_plugin_ledger::AgentName::new("terra")),
        class: MailClass::Wake,
        text: format!("ask {id}"),
        subject: format!("ask {id}"),
        cites: Vec::new(),
        refs: BTreeSet::new(),
        mail_seq: seq.map(Seq),
        at: at(0),
    }
}

/// A `mail/delivered` step, as the router would have written it.
pub fn delivered(seq: u64, wake: &WakeId, class: &str, summary: &str) -> Step {
    step(
        seq,
        wake,
        "mail/delivered",
        serde_json::json!({
            "class": class,
            "from": "gh:o/r#1",
            "subject": "s",
            "summary": summary,
        }),
    )
}

/// A `wake/end` step carrying a consumed set.
pub fn wake_end(seq: u64, wake: &WakeId, reason: &str, consumed: &[(u64, u64)]) -> Step {
    let ranges: Vec<serde_json::Value> = consumed
        .iter()
        .map(|(f, t)| serde_json::json!({ "from": f, "to": t }))
        .collect();
    step(
        seq,
        wake,
        "wake/end",
        serde_json::json!({ "reason": reason, "cause": null, "consumed": ranges }),
    )
}
