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

/// The `class:` namespace P5-D3 spells a wake class in.
pub const CLASS_PREFIX: &str = "class:";

/// One wake class in its canonical spelling. A row may configure `ask` or `class:ask`; both mean
/// the same class, and neither is a prefix test.
pub fn canonical_class(raw: &str) -> String {
    if raw.starts_with(CLASS_PREFIX) {
        raw.to_string()
    } else {
        format!("{CLASS_PREFIX}{raw}")
    }
}

/// PURE: the wake classes a trigger's refs carry — its refs in the `class:` namespace, canonical.
pub fn classes_of(refs: &BTreeSet<bough_plugin_ledger::Ref>) -> BTreeSet<String> {
    refs.iter()
        .map(|r| r.as_str())
        .filter(|r| r.starts_with(CLASS_PREFIX))
        .map(canonical_class)
        .collect()
}

/// PURE. Matching against `wake_classes` is an INTERSECTION, never a prefix test: `class:asks`
/// must not reactivate a row that asked for `class:ask`.
pub fn admits(
    dormant: bool,
    kind: WakeKind,
    trigger: Option<&TriggerFacts>,
    wake_classes: &BTreeSet<String>,
) -> Decision {
    // A live agent admits EVERY kind: dormancy is the only thing this listener has an opinion
    // about, and a kind-based rule here would be a scheduler hiding in an admission point.
    let _ = kind;
    if !dormant {
        return Decision::Admit;
    }
    let Some(trigger) = trigger else {
        // No trigger at all is a tick, a catch-up or a drain: exactly what §1 says a dormant
        // agent does not get.
        return Decision::Defer(crate::PLUGIN_NAME);
    };
    if trigger.from_andrey {
        return Decision::Reactivate(ReactivateCause::Andrey);
    }
    let wanted: BTreeSet<String> = wake_classes.iter().map(|c| canonical_class(c)).collect();
    let carried = classes_of(&trigger.refs);
    if carried.intersection(&wanted).next().is_some() {
        return Decision::Reactivate(ReactivateCause::WakeClass);
    }
    Decision::Defer(crate::PLUGIN_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_agents::{MailClass, MessageId};
    use bough_plugin_ledger::Ref;

    fn classes(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|n| n.to_string()).collect()
    }

    fn trigger(from_andrey: bool, class: MailClass, refs: &[&str]) -> TriggerFacts {
        TriggerFacts {
            message: MessageId::new("m1"),
            from_andrey,
            class,
            refs: refs.iter().map(|r| Ref::new(*r)).collect(),
            mail_seq: None,
        }
    }

    /// Dormancy is the ONLY thing this listener suppresses.
    #[test]
    fn a_live_agent_admits_every_kind() {
        for kind in [
            WakeKind::Answer,
            WakeKind::Drain,
            WakeKind::Scheduled,
            WakeKind::Catchup,
            WakeKind::Task,
        ] {
            assert_eq!(
                admits(false, kind, None, &classes(&[])),
                Decision::Admit,
                "{kind:?} on a live agent"
            );
            assert_eq!(
                admits(
                    false,
                    kind,
                    Some(&trigger(false, MailClass::Ordinary, &[])),
                    &classes(&[])
                ),
                Decision::Admit
            );
        }
    }

    #[test]
    fn a_dormant_agent_defers_a_drain() {
        assert_eq!(
            admits(true, WakeKind::Drain, None, &classes(&["class:ask"])),
            Decision::Defer(crate::PLUGIN_NAME)
        );
        // Ordinary mail with no class ref is the backlog case: it queues, it does not wake.
        assert_eq!(
            admits(
                true,
                WakeKind::Drain,
                Some(&trigger(false, MailClass::Ordinary, &["gh:o/r#1"])),
                &classes(&["class:ask"])
            ),
            Decision::Defer(crate::PLUGIN_NAME)
        );
    }

    #[test]
    fn a_dormant_agent_defers_a_tick_and_a_catch_up() {
        for kind in [WakeKind::Scheduled, WakeKind::Catchup] {
            assert_eq!(
                admits(true, kind, None, &classes(&["class:ask"])),
                Decision::Defer(crate::PLUGIN_NAME),
                "{kind:?} on a dormant agent"
            );
        }
    }

    #[test]
    fn andrey_always_reactivates_whatever_the_classes_say() {
        assert_eq!(
            admits(
                true,
                WakeKind::Answer,
                Some(&trigger(true, MailClass::Wake, &[])),
                &classes(&[]),
            ),
            Decision::Reactivate(ReactivateCause::Andrey)
        );
        // Even an ORDINARY-class message from him: §1 says Andrey, not Andrey-if-urgent.
        assert_eq!(
            admits(
                true,
                WakeKind::Drain,
                Some(&trigger(true, MailClass::Ordinary, &["class:never"])),
                &classes(&["class:ask"]),
            ),
            Decision::Reactivate(ReactivateCause::Andrey)
        );
    }

    #[test]
    fn a_configured_wake_class_ref_reactivates() {
        assert_eq!(
            admits(
                true,
                WakeKind::Catchup,
                Some(&trigger(false, MailClass::Wake, &["class:ask", "gh:o/r#7"])),
                &classes(&["class:ask"]),
            ),
            Decision::Reactivate(ReactivateCause::WakeClass)
        );
        // A row may spell its classes bare; the canonical form is what matches.
        assert_eq!(
            admits(
                true,
                WakeKind::Catchup,
                Some(&trigger(false, MailClass::Wake, &["class:ask"])),
                &classes(&["ask"]),
            ),
            Decision::Reactivate(ReactivateCause::WakeClass)
        );
    }

    #[test]
    fn an_unconfigured_class_defers() {
        assert_eq!(
            admits(
                true,
                WakeKind::Catchup,
                Some(&trigger(false, MailClass::Wake, &["class:review"])),
                &classes(&["class:ask"]),
            ),
            Decision::Defer(crate::PLUGIN_NAME)
        );
        // No configured class at all: nothing but Andrey gets through.
        assert_eq!(
            admits(
                true,
                WakeKind::Catchup,
                Some(&trigger(false, MailClass::Wake, &["class:ask"])),
                &classes(&[]),
            ),
            Decision::Defer(crate::PLUGIN_NAME)
        );
    }

    #[test]
    fn wake_class_matching_is_an_intersection_not_a_prefix() {
        assert_eq!(
            admits(
                true,
                WakeKind::Catchup,
                Some(&trigger(false, MailClass::Wake, &["class:asks"])),
                &classes(&["class:ask"]),
            ),
            Decision::Defer(crate::PLUGIN_NAME),
            "`class:asks` is not `class:ask`"
        );
        assert_eq!(
            admits(
                true,
                WakeKind::Catchup,
                Some(&trigger(false, MailClass::Wake, &["class:ask"])),
                &classes(&["class:asks"]),
            ),
            Decision::Defer(crate::PLUGIN_NAME),
            "and the test is symmetric"
        );
        // A ref outside the namespace never matches, however it is spelled.
        assert_eq!(
            admits(
                true,
                WakeKind::Catchup,
                Some(&trigger(false, MailClass::Wake, &["ask"])),
                &classes(&["class:ask"]),
            ),
            Decision::Defer(crate::PLUGIN_NAME)
        );
    }
}
