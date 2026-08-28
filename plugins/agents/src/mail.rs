//! Invariant (§2): EVERY inbox mutation is a durable `inbox/spliced` step keyed by the message
//! id. The live inbox is a cache of that fold (P2-D8): `Inbox::rebuild` is the same function
//! resume and crash repair use, so the two copies cannot drift.

use std::collections::BTreeSet;

use bough_plugin_ledger::vocabulary::{InboxSpliced, SpliceOp, SpliceTarget};
use bough_plugin_ledger::{
    Append, Cite, Class, LedgerHandle, Ref, Seq, Step, StepId, StepType, TrajId,
};
use chrono::{DateTime, Utc};
use parking_lot::Mutex;

use crate::error::AgentError;
use crate::ids::{AgentId, MessageId, WorkerId};

pub use bough_plugin_ledger::vocabulary::MailClass;
use bough_plugin_ledger::AgentName;

/// Which queue a message lands in (§2).
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Target {
    /// Delivered at the START of the next wake.
    NextWake,
    /// Delivered at the next STEP boundary of the running wake.
    NextStep,
}

impl Target {
    pub(crate) fn wire(self) -> SpliceTarget {
        match self {
            Target::NextWake => SpliceTarget::NextWake,
            Target::NextStep => SpliceTarget::NextStep,
        }
    }
    pub(crate) fn of_wire(w: SpliceTarget) -> Target {
        match w {
            SpliceTarget::NextWake => Target::NextWake,
            SpliceTarget::NextStep => Target::NextStep,
        }
    }
}

/// Who sent a message. `Andrey` is the one sender that changes the wake's class (§5).
#[derive(Clone, Debug, PartialEq)]
pub enum Sender {
    Andrey,
    Agent(AgentName),
    Worker(WorkerId),
    /// A collector row (Phase 6); named here so the vocabulary is complete.
    Collector(String),
    /// A ward file (§9), by its name. MERGE (note 7): runtime code used to post as
    /// `System("ward:<name>")`, which leaked an interned `&'static str` per distinct ward file.
    Ward(String),
    /// A hook executable (§9), by its point name. Same reason as [`Sender::Ward`].
    Hook(String),
    /// The harness itself: crash repair, a schedule firing, a bound being hit.
    System(&'static str),
}

/// The wire form of [`Sender`]: the durable splice must round-trip through the ledger, and
/// `&'static str` cannot be deserialized. Interning (below) closes the gap without changing §2's
/// spelling of the enum.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct SenderWire {
    kind: String,
    #[serde(default)]
    name: Option<String>,
}

/// Distinct system tags seen so far. A system tag is a compile-time constant in every caller, so
/// the set is small and fixed; interning is what lets `Sender::System(&'static str)` survive a
/// round trip through the ledger without a per-message leak.
static SYSTEM_TAGS: Mutex<BTreeSet<&'static str>> = Mutex::new(BTreeSet::new());

fn intern(s: &str) -> &'static str {
    let mut tags = SYSTEM_TAGS.lock();
    if let Some(found) = tags.iter().find(|t| **t == s) {
        return found;
    }
    let leaked: &'static str = Box::leak(s.to_string().into_boxed_str());
    tags.insert(leaked);
    leaked
}

impl Sender {
    fn wire(&self) -> SenderWire {
        let (kind, name) = match self {
            Sender::Andrey => ("andrey", None),
            Sender::Agent(a) => ("agent", Some(a.to_string())),
            Sender::Worker(w) => ("worker", Some(w.to_string())),
            Sender::Collector(c) => ("collector", Some(c.clone())),
            Sender::Ward(w) => ("ward", Some(w.clone())),
            Sender::Hook(h) => ("hook", Some(h.clone())),
            Sender::System(s) => ("system", Some((*s).to_string())),
        };
        SenderWire {
            kind: kind.to_string(),
            name,
        }
    }
    fn of_wire(w: &SenderWire) -> Sender {
        let name = w.name.clone().unwrap_or_default();
        match w.kind.as_str() {
            "agent" => Sender::Agent(AgentName::new(&name)),
            "worker" => Sender::Worker(WorkerId::new(&name)),
            "collector" => Sender::Collector(name),
            "ward" => Sender::Ward(name),
            "hook" => Sender::Hook(name),
            "system" => Sender::System(intern(&name)),
            _ => Sender::Andrey,
        }
    }
    /// The `from` ref a message cites itself by.
    pub fn as_ref_str(&self) -> String {
        match self {
            Sender::Andrey => "andrey".to_string(),
            Sender::Agent(a) => format!("agent:{a}"),
            Sender::Worker(w) => format!("worker:{w}"),
            Sender::Collector(c) => format!("collector:{c}"),
            Sender::Ward(w) => format!("ward:{w}"),
            Sender::Hook(h) => format!("hook:{h}"),
            Sender::System(s) => format!("system:{s}"),
        }
    }
}

/// One piece of mail.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub id: MessageId,
    pub from: Sender,
    /// The ledger's two urgencies (§5).
    pub class: MailClass,
    pub text: String,
    pub subject: String,
    pub cites: Vec<Cite>,
    pub refs: BTreeSet<Ref>,
    /// Set when this is DELIVERED mail with a `mail/delivered` step. Consumption is per
    /// (agent, seq) and applies to delivered mail only (§5).
    pub mail_seq: Option<Seq>,
    pub at: DateTime<Utc>,
}

/// The durable envelope of a [`Message`], carried by the `inbox/spliced { op: insert }` step.
///
/// It is an ADDITIVE sibling of the ledger's `InboxSpliced` body (whose fields carry only the
/// message id, the op, the target and the wake flag). Without it `Inbox::rebuild` could not
/// reconstruct a message and P2-D8 — "the live inbox is a cache of the fold" — would be
/// unimplementable. See the WP-2 report.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct MessagePayload {
    id: String,
    from: SenderWire,
    class: MailClass,
    text: String,
    subject: String,
    cites: Vec<Cite>,
    refs: BTreeSet<Ref>,
    #[serde(default)]
    mail_seq: Option<Seq>,
    /// RFC 3339. `chrono`'s serde feature is not on in this workspace, and the ledger's own
    /// bodies carry no timestamps, so the seam spells it out rather than turning a feature on.
    at: String,
}

impl MessagePayload {
    fn of(msg: &Message) -> MessagePayload {
        MessagePayload {
            id: msg.id.to_string(),
            from: msg.from.wire(),
            class: msg.class,
            text: msg.text.clone(),
            subject: msg.subject.clone(),
            cites: msg.cites.clone(),
            refs: msg.refs.clone(),
            mail_seq: msg.mail_seq,
            at: msg.at.to_rfc3339(),
        }
    }
    fn into_message(self) -> Message {
        Message {
            id: MessageId::new(&self.id),
            from: Sender::of_wire(&self.from),
            class: self.class,
            text: self.text,
            subject: self.subject,
            cites: self.cites,
            refs: self.refs,
            mail_seq: self.mail_seq,
            at: DateTime::parse_from_rfc3339(&self.at)
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|_| Utc::now()),
        }
    }
}

impl Message {
    /// The one predicate §5's "an Andrey message ALWAYS gets a fresh sol answer wake" turns on.
    pub fn is_andrey(&self) -> bool {
        matches!(self.from, Sender::Andrey)
    }

    /// A plain ordinary-class message from `from`. The convenience every caller of `send` wants.
    pub fn new(from: Sender, subject: &str, text: &str, at: DateTime<Utc>) -> Message {
        Message {
            id: MessageId::new(uuid::Uuid::now_v7().to_string()),
            from,
            class: MailClass::Ordinary,
            text: text.to_string(),
            subject: subject.to_string(),
            cites: Vec::new(),
            refs: BTreeSet::new(),
            mail_seq: None,
            at,
        }
    }

    /// The same, at [`MailClass::Wake`] urgency.
    pub fn waking(from: Sender, subject: &str, text: &str, at: DateTime<Utc>) -> Message {
        Message {
            class: MailClass::Wake,
            ..Message::new(from, subject, text, at)
        }
    }
}

/// What an inbox mutation produced: the durable step, and where the message went.
#[derive(Clone, Debug, PartialEq)]
pub struct InboxReceipt {
    pub message: MessageId,
    pub agent: AgentId,
    pub target: Target,
    /// Whether the sender asked for a wake. The driver decides what to do with it.
    pub wake: bool,
    pub step: StepId,
    pub seq: Seq,
}

/// A message the driver has claimed for a wake, with the claim's durable step.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedMessage {
    pub message: Message,
    pub target: Target,
    pub claim_step: StepId,
}

/// The wake id an insert is attributed to. An insert happens OUTSIDE any wake — the sender is not
/// the agent — and `wake` is mandatory on every step (§3), so the seam names that fact rather than
/// borrowing an unrelated wake's id.
pub fn outside_wake() -> bough_plugin_ledger::WakeId {
    bough_plugin_ledger::WakeId::new("wake:outside")
}

fn splice_kind() -> StepType {
    StepType::new("inbox/spliced")
}

/// One agent's two queues, backed by the durable fold.
pub struct Inbox {
    ledger: LedgerHandle,
    traj: TrajId,
    agent: AgentId,
    queues: Mutex<Vec<(Message, Target)>>,
}

impl Inbox {
    /// An empty inbox wired to the agent's trajectory.
    pub fn new(ledger: LedgerHandle, traj: TrajId, agent: AgentId) -> Inbox {
        Inbox {
            ledger,
            traj,
            agent,
            queues: Mutex::new(Vec::new()),
        }
    }

    /// Seed the live cache from a rebuilt fold, without re-appending anything.
    pub(crate) fn seed(&self, items: Vec<(Message, Target)>) {
        *self.queues.lock() = items;
    }

    /// Insert one message, appending its `inbox/spliced { op: insert }` step first.
    pub async fn insert(&self, msg: Message, target: Target) -> Result<InboxReceipt, AgentError> {
        self.insert_waking(msg, target, false).await
    }

    /// [`Inbox::insert`] carrying the sender's wake flag, which the durable body records and the
    /// receipt hands to the driver. (§2 pairs every mutation with a `wake` flag; the plan's
    /// `insert` signature omits it, so this is the one the seam itself calls.)
    pub async fn insert_waking(
        &self,
        msg: Message,
        target: Target,
        wake: bool,
    ) -> Result<InboxReceipt, AgentError> {
        let mut body = serde_json::to_value(InboxSpliced {
            message: msg.id.to_string(),
            op: SpliceOp::Insert,
            target: target.wire(),
            wake,
        })
        .expect("InboxSpliced serializes");
        let payload = serde_json::to_value(MessagePayload::of(&msg)).expect("payload serializes");
        body.as_object_mut()
            .expect("an object body")
            .insert("payload".to_string(), payload);

        let step = self
            .ledger
            .0
            .append(Append {
                traj: self.traj.clone(),
                wake: outside_wake(),
                kind: splice_kind(),
                class: Class::Thought,
                body,
                cites: msg.cites.clone(),
                at: msg.at,
                id: None,
            })
            .await?;

        self.queues.lock().push((msg.clone(), target));
        Ok(InboxReceipt {
            message: msg.id,
            agent: self.agent.clone(),
            target,
            wake,
            step: step.id,
            seq: step.seq,
        })
    }

    /// A pure DELETION splice: one durable step per removed message. Removes from the live cache
    /// AND appends; [`Inbox::take`] plus [`Inbox::append_removal`] is the two-phase form a
    /// concurrent claim needs.
    pub(crate) async fn remove(
        &self,
        id: &MessageId,
        target: Target,
        op: SpliceOp,
        wake: bough_plugin_ledger::WakeId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<StepId, AgentError> {
        let step = self
            .append_removal(id, target, op, wake, at, reason)
            .await?;
        self.queues.lock().retain(|(m, _)| m.id != *id);
        Ok(step)
    }

    /// Append the deletion splice WITHOUT touching the live cache: the caller already took the
    /// message out of it under the queue lock.
    pub(crate) async fn append_removal(
        &self,
        id: &MessageId,
        target: Target,
        op: SpliceOp,
        wake: bough_plugin_ledger::WakeId,
        at: DateTime<Utc>,
        reason: Option<&str>,
    ) -> Result<StepId, AgentError> {
        let mut body = serde_json::to_value(InboxSpliced {
            message: id.to_string(),
            op,
            target: target.wire(),
            wake: false,
        })
        .expect("InboxSpliced serializes");
        if let Some(reason) = reason {
            body.as_object_mut()
                .expect("an object body")
                .insert("reason".to_string(), serde_json::json!(reason));
        }
        let step = self
            .ledger
            .0
            .append(Append {
                traj: self.traj.clone(),
                wake,
                kind: splice_kind(),
                class: Class::Thought,
                body,
                cites: vec![],
                at,
                id: None,
            })
            .await?;
        Ok(step.id)
    }

    /// Splice one seed message back out when a creation transaction rolls back after `attach`.
    pub(crate) async fn discard_seed(
        &self,
        id: &MessageId,
        target: Target,
        at: DateTime<Utc>,
    ) -> Result<StepId, AgentError> {
        self.remove(
            id,
            target,
            SpliceOp::Discard,
            outside_wake(),
            at,
            Some("the creation transaction rolled back"),
        )
        .await
    }

    /// Select AND remove in one critical section: the atomic half of a concurrent claim.
    pub(crate) fn take(&self, sel: &crate::factory::ClaimSelector) -> Vec<Message> {
        let mut queues = self.queues.lock();
        let chosen = Self::admitted(&queues[..], sel);
        queues.retain(|(m, _)| !chosen.iter().any(|c| c.id == m.id));
        chosen
    }

    /// The messages a selector admits, oldest first, without removing anything. Read-only: a
    /// CLAIM goes through [`Inbox::take`], which selects and removes under one lock.
    pub fn select(&self, sel: &crate::factory::ClaimSelector) -> Vec<Message> {
        let queues = self.queues.lock();
        Self::admitted(&queues[..], sel)
    }

    /// The selector's pure filter over a locked queue.
    fn admitted(queues: &[(Message, Target)], sel: &crate::factory::ClaimSelector) -> Vec<Message> {
        let mut out: Vec<Message> = queues
            .iter()
            .filter(|(m, t)| {
                *t == sel.target
                    && sel.classes.as_ref().is_none_or(|cs| cs.contains(&m.class))
                    && sel.only.as_ref().is_none_or(|ids| ids.contains(&m.id))
                    && !(sel.exclude_andrey && m.is_andrey())
            })
            .map(|(m, _)| m.clone())
            .collect();
        if let Some(only) = &sel.only {
            // The selector's order is authoritative when it names messages explicitly.
            out.sort_by_key(|m| only.iter().position(|i| *i == m.id).unwrap_or(usize::MAX));
        }
        if let Some(limit) = sel.limit {
            out.truncate(limit);
        }
        out
    }

    /// Pending messages for one queue, oldest first.
    pub fn pending(&self, target: Target) -> Vec<Message> {
        self.queues
            .lock()
            .iter()
            .filter(|(_, t)| *t == target)
            .map(|(m, _)| m.clone())
            .collect()
    }

    /// Whether that queue has anything.
    pub fn has(&self, target: Target) -> bool {
        self.queues.lock().iter().any(|(_, t)| *t == target)
    }

    /// Total pending across both queues.
    pub fn len(&self) -> usize {
        self.queues.lock().len()
    }

    /// Whether both queues are empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop everything pending WITHOUT a durable splice. Only teardown uses it: the agent is
    /// going away, and a discard step for mail nobody will ever read is noise, not a record.
    pub(crate) fn clear_live(&self) {
        self.queues.lock().clear();
    }

    /// The pure fold over `inbox/spliced` steps: insert minus claim minus discard. Used at
    /// resume and by crash repair, so the live inbox and the ledger can never disagree (P2-D8).
    pub fn rebuild(steps: &[Step]) -> Vec<(Message, Target)> {
        let mut out: Vec<(Message, Target)> = Vec::new();
        let mut ordered: Vec<&Step> = steps
            .iter()
            .filter(|s| s.kind.as_str() == "inbox/spliced")
            .collect();
        ordered.sort_by_key(|s| s.seq);
        for step in ordered {
            let Ok(splice) = serde_json::from_value::<InboxSpliced>((*step.body).clone()) else {
                continue;
            };
            match splice.op {
                SpliceOp::Insert => {
                    let Some(payload) = step.body.get("payload") else {
                        continue;
                    };
                    let Ok(payload) = serde_json::from_value::<MessagePayload>(payload.clone())
                    else {
                        continue;
                    };
                    out.push((payload.into_message(), Target::of_wire(splice.target)));
                }
                SpliceOp::Claim | SpliceOp::Discard => {
                    out.retain(|(m, _)| m.id.as_str() != splice.message);
                }
            }
        }
        out
    }
}

/// One piece of DELIVERED mail, as a producer hands it to [`crate::Agent::deliver`] (P3-D15).
///
/// The `mail/delivered` step is EVIDENCE, so `cites` is what makes it appendable at all: mail that
/// cannot say where it came from is not deliverable.
#[derive(Clone, Debug, PartialEq)]
pub struct Delivery {
    pub from: Sender,
    pub class: MailClass,
    pub subject: String,
    pub summary: String,
    pub text: String,
    pub cites: Vec<Cite>,
    pub refs: BTreeSet<Ref>,
    pub at: DateTime<Utc>,
}
