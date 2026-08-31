//! Invariant: an [`Envelope`] carries NO recipient, ever. Choosing recipients is this crate's
//! whole job (§3: "mail delivery is the one eager step"), so a producer that could name one would
//! be a second router.

use std::collections::BTreeSet;

use bough_plugin_agents::{InboxReceipt, MailClass, Sender};
use bough_plugin_ledger::{AgentName, Cite, Ref, StepId, TrajId};
use chrono::{DateTime, Utc};

/// One piece of mail as a PRODUCER hands it over.
#[derive(Clone, Debug)]
pub struct Envelope {
    pub from: Sender,
    /// §5's two urgencies. [`MailClass::Wake`] is what may reactivate a dormant agent, gated by
    /// `refs` against the row's `wake_classes` (P5-D3).
    pub class: MailClass,
    pub subject: String,
    pub summary: String,
    pub text: String,
    pub cites: Vec<Cite>,
    /// The routing key. A wake CLASS is a ref in the `class:` namespace (P5-D3).
    pub refs: BTreeSet<Ref>,
    /// MERGE (track B → Phase 5): the AT-LEAST-ONCE guard, for a producer that may re-offer the
    /// same world item — a collector sweep whose watermark write was lost. When set, a recipient
    /// whose trajectory already carries a `mail/delivered` step CITING this ref is skipped, and
    /// says so in [`RouteReport::deduped`] rather than being delivered to twice.
    ///
    /// The guard lives HERE and not in the producer because the router is what chooses
    /// recipients: a collector that hands over an envelope does not know who will get it.
    pub dedupe_on: Option<Ref>,
    pub at: DateTime<Utc>,
}

/// What one [`crate::MailHandle::route`] did. `delivered` is per-agent: one `mail/delivered` step,
/// one seq, one consumption state each (§3, §5).
#[derive(Clone, Debug)]
pub struct RouteReport {
    pub matched: Vec<AgentName>,
    pub delivered: Vec<(AgentName, InboxReceipt)>,
    /// Matched lanes with no live handle. They are in `matched` because the REFS matched, and
    /// here because nothing was delivered to them: a caller that reads only `matched` would
    /// otherwise be told an event was routed when it was not.
    pub undeliverable: Vec<AgentName>,
    /// The `mail/unrouted` step on the unsorted trajectory: `Some` when nobody matched, and also
    /// `Some` when everyone who matched was `undeliverable`, so the event stays recoverable.
    pub unsorted: Option<StepId>,
    /// `true` iff an unsorted sink was mounted and took it as ordinary mail.
    pub adopted: bool,
    /// Matched lanes that already carried [`Envelope::dedupe_on`] and were therefore NOT
    /// delivered to again. Reported, because "delivered nothing" and "delivered nothing because
    /// it was already there" are different facts.
    pub deduped: Vec<AgentName>,
}

/// What one [`crate::MailHandle::link_ref`] / `unlink_ref` did.
#[derive(Clone, Debug, PartialEq)]
pub struct LinkReport {
    pub agent: AgentName,
    pub added: BTreeSet<Ref>,
    pub removed: BTreeSet<Ref>,
    /// ALWAYS 0. §5: "a late-added routing ref starts mail delivery from link time, with earlier
    /// history reachable by query, never queued as backlog." Named in the report so the rule is
    /// asserted rather than assumed.
    pub backfilled: usize,
    /// Trajectories the new ref now reaches through `connected()`, for the caller to show.
    pub now_connected: Vec<TrajId>,
}

/// A question only Andrey (through the leader) can settle (§4).
#[derive(Clone, Debug)]
pub struct Question {
    pub asked_by: &'static str,
    pub about: String,
    pub options: Vec<String>,
    pub cites: Vec<Cite>,
    pub refs: BTreeSet<Ref>,
    pub at: DateTime<Utc>,
}
