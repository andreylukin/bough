//! Invariant: [`admits`] is PURE and TOTAL over its inputs — no clock, no ledger — and it is the
//! whole of §1's activation rule: a dormant agent gets no ticks and no drain wakes, is reactivated
//! only by an Andrey message or a wake-class item per `agents.wake_classes`, and ordinary mail
//! queues silently in the meantime.

use std::collections::BTreeSet;

use bough_plugin_agents::{TriggerFacts, WakeKind};

/// Why a reactivation happened.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ReactivateCause {
    /// §1: Andrey always reactivates, whatever the wake classes say.
    Andrey,
    /// A wake-class item whose `class:` refs intersect the row's `wake_classes` (P5-D3).
    WakeClass,
    /// `/wake` from the surface.
    Command,
}

/// What the admission listener decided.
#[derive(Clone, Debug, PartialEq)]
pub enum Decision {
    /// Not dormant, or a wake that must run anyway.
    Admit,
    /// Dormant, and this trigger reactivates: the caller writes the step, then admits.
    Reactivate(ReactivateCause),
    /// Dormant: no wake. Ordinary mail stays queued and unconsumed ON PURPOSE.
    Defer(&'static str),
}

/// PURE. Matching against `wake_classes` is an INTERSECTION, never a prefix test: `class:asks`
/// must not be reactivated by a row that asked for `class:ask`.
pub fn admits(
    _dormant: bool,
    _kind: WakeKind,
    _trigger: Option<&TriggerFacts>,
    _wake_classes: &BTreeSet<String>,
) -> Decision {
    todo!("WP-2: §1's activation rule, total over its inputs")
}
