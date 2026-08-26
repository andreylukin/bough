//! Invariant (§5): mail consumption is the UNION of the `wake/end` consumed sets, order
//! independent; a drain wake consumes only ORDINARY seqs and an answer wake only its trigger;
//! ONE drain wake is in flight per agent; and at every `wake_end`, unconsumed ordinary mail
//! implies a scheduled drain wake. That last one is the STANDING invariant — it is checked after
//! every wake, not only in a test.
//!
//! P2-D16: urgency and drain scheduling are modules here rather than a `wake-scheduler` row.
//! §5 names one, but there is no second implementation in sight and §0.2 forbids splitting
//! preemptively. Flagged for the §15-item-6 review at phase close.

use bough_plugin_agents::{ClaimSelector, MailClass, Message, MessageId, Target};
use bough_plugin_ledger::{Seq, SeqRange, Step};
use bough_plugin_llm::WakeKind;

/// How urgently a message wants a wake (§5's two urgencies).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Urgency {
    /// An Andrey message or wake-class mail: wake NOW.
    Immediate,
    /// Ordinary mail: a debounced drain wake, unless another wake drains it first.
    Coalesced,
}

impl Urgency {
    /// The durable spelling on `wake/start`.
    pub fn durable(&self, kind: WakeKind) -> bough_plugin_ledger::vocabulary::Urgency {
        use bough_plugin_ledger::vocabulary::Urgency as D;
        match (self, kind) {
            (_, WakeKind::Scheduled) => D::Scheduled,
            (_, WakeKind::Catchup) => D::Catchup,
            (Urgency::Immediate, _) => D::Immediate,
            (Urgency::Coalesced, _) => D::Coalesced,
        }
    }
}

/// Decide the urgency of one message.
///
/// The `wake` flag is the SENDER's request, not the decision: an `inject` (no wake) never starts
/// a wake, and everything else is decided by the message's own class. Andrey is called out
/// separately because §5 makes his message immediate whatever queue it arrived through.
pub fn urgency_of(msg: &Message, _target: Target, wake_flag: bool) -> Urgency {
    if msg.is_andrey() {
        return Urgency::Immediate;
    }
    match (msg.class, wake_flag) {
        (MailClass::Wake, true) => Urgency::Immediate,
        _ => Urgency::Coalesced,
    }
}

/// What arriving mail asks the driver to do. Pure; the driver owns the timers.
#[derive(Clone, Debug, PartialEq)]
pub enum Schedule {
    /// Start a wake of this kind NOW, triggered by this message.
    Now { kind: WakeKind, trigger: MessageId },
    /// Join (or open) the debounce window for the drain wake.
    Debounce,
    /// The message waits for whatever wakes the agent next: an `inject`, or ordinary mail while a
    /// drain wake is already in flight (§5: one drain wake in flight per agent).
    Wait,
}

/// §5's scheduling rules, as one pure function.
///
/// * An Andrey message ALWAYS gets a fresh Answer wake, whatever queue it arrived through.
/// * Wake-class mail from anyone else wakes a dormant agent as a Catchup wake.
/// * Ordinary mail schedules the debounced drain — unless one is already in flight, in which
///   case the seqs simply stay unconsumed and that wake will take them (or the standing
///   invariant will schedule the next one).
/// * A message the sender did not ask to wake on (`inject`) never schedules anything.
pub fn schedule_for(
    msg: &Message,
    target: Target,
    wake_flag: bool,
    drain_in_flight: bool,
) -> Schedule {
    if msg.is_andrey() {
        return Schedule::Now {
            kind: WakeKind::Answer,
            trigger: msg.id.clone(),
        };
    }
    if !wake_flag {
        return Schedule::Wait;
    }
    match urgency_of(msg, target, wake_flag) {
        Urgency::Immediate => Schedule::Now {
            kind: WakeKind::Catchup,
            trigger: msg.id.clone(),
        },
        Urgency::Coalesced if drain_in_flight => Schedule::Wait,
        Urgency::Coalesced => Schedule::Debounce,
    }
}

/// The claim selector for a wake of this kind (§5).
///
/// A DRAIN wake claims ordinary seqs only; an answer wake claims its trigger only, which the
/// caller narrows with [`only_the_trigger`]; between steps, only `next-step` input is claimed and
/// the class filter does not apply, because a steer is not delivered mail.
pub fn selector_for(kind: WakeKind, target: Target) -> ClaimSelector {
    let classes = match (kind, target) {
        (_, Target::NextStep) => None,
        (WakeKind::Drain, _) => Some(vec![MailClass::Ordinary]),
        _ => None,
    };
    ClaimSelector {
        target,
        only: None,
        classes,
        limit: None,
    }
}

/// Narrow a selector to exactly one message: what an answer wake claims at its first step.
pub fn only_the_trigger(mut sel: ClaimSelector, trigger: &MessageId) -> ClaimSelector {
    sel.only = Some(vec![trigger.clone()]);
    sel
}

/// Whether a wake of this kind may claim this message. The predicate behind "a drain wake never
/// answers Andrey": his message is wake-class, and a drain claims ordinary only.
pub fn admits(kind: WakeKind, msg: &Message) -> bool {
    match kind {
        WakeKind::Drain => msg.class == MailClass::Ordinary && !msg.is_andrey(),
        _ => true,
    }
}

/// The union of consumed sets across `wake/end` steps, order independent (§5).
pub fn consumed_union(wake_ends: &[Step]) -> Vec<SeqRange> {
    let mut all: Vec<SeqRange> = Vec::new();
    for step in wake_ends {
        if step.kind.as_str() != "wake/end" {
            continue;
        }
        if let Some(list) = step.body.get("consumed").and_then(|v| v.as_array()) {
            for r in list {
                if let Ok(range) = serde_json::from_value::<SeqRange>(r.clone()) {
                    all.push(range);
                }
            }
        }
    }
    SeqRange::union(&all)
}

/// Delivered mail not covered by the consumed union, in seq order.
pub fn unconsumed(delivered: &[Step], consumed: &[SeqRange]) -> Vec<Step> {
    let mut out: Vec<Step> = delivered
        .iter()
        .filter(|s| s.kind.as_str() == "mail/delivered")
        .filter(|s| !consumed.iter().any(|r| r.contains(s.seq)))
        .cloned()
        .collect();
    out.sort_by_key(|s| s.seq);
    out
}

/// Whether a delivered-mail step is ORDINARY class (the only class a drain wake takes).
pub fn is_ordinary(step: &Step) -> bool {
    step.body.get("class").and_then(|v| v.as_str()) == Some("ordinary")
}

/// The standing invariant, as a pure predicate: given the unconsumed ordinary mail and whether a
/// drain wake is scheduled, is the loop in a legal state?
pub fn standing_invariant_holds(unconsumed_ordinary: usize, drain_scheduled: bool) -> bool {
    unconsumed_ordinary == 0 || drain_scheduled
}

/// One agent's drain gate: §5's "one drain wake in flight per agent", as data rather than as a
/// comment. `arm` is what a debounce window asks for; only the first caller wins until the wake
/// that took the slot releases it.
#[derive(Debug, Default)]
pub struct DrainGate {
    in_flight: std::sync::atomic::AtomicBool,
}

impl DrainGate {
    pub fn new() -> DrainGate {
        DrainGate::default()
    }
    /// `true` if this caller now owns the one drain slot.
    pub fn arm(&self) -> bool {
        self.in_flight
            .compare_exchange(
                false,
                true,
                std::sync::atomic::Ordering::SeqCst,
                std::sync::atomic::Ordering::SeqCst,
            )
            .is_ok()
    }
    pub fn release(&self) {
        self.in_flight
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }
    pub fn in_flight(&self) -> bool {
        self.in_flight.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// The seqs a wake consumed, as ranges: the claimed messages that were DELIVERED mail (§5:
/// consumption applies to delivered mail only).
pub fn consumed_of(claimed: &[bough_plugin_agents::ClaimedMessage]) -> Vec<SeqRange> {
    let ranges: Vec<SeqRange> = claimed
        .iter()
        .filter_map(|c| c.message.mail_seq)
        .map(|s: Seq| SeqRange { from: s, to: s })
        .collect();
    SeqRange::union(&ranges)
}
