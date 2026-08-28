//! Invariant: a collected item becomes CITED mail by construction. `delivery_of` is PURE and
//! always produces at least one cite — the item's own ref — so no collector can deliver an
//! uncitable claim (§0.2, §3).

use bough_plugin_agents::{Delivery, Sender};
use bough_plugin_ledger::Cite;
use bough_plugin_mail_router::Envelope;

use crate::Collected;

/// PURE: one collected item becomes one [`Delivery`], cited by construction.
pub fn delivery_of(item: &Collected, collector: &str) -> Delivery {
    let mut refs = item.refs.clone();
    // The item's OWN ref is always in `refs`, whatever the parser put there: it is what the
    // dedupe guard and Phase 5's `mail-router` key on.
    refs.insert(item.r#ref.clone());
    Delivery {
        from: Sender::Collector(collector.to_string()),
        class: item.class,
        subject: item.subject.clone(),
        summary: item.summary.clone(),
        text: item.text.clone(),
        cites: vec![Cite {
            r#ref: item.r#ref.clone(),
            url: item.url.clone().filter(|u| !u.trim().is_empty()),
        }],
        refs,
        at: item.at,
    }
}

/// PURE: the same item as an [`Envelope`] for `mail-router`, cited by construction.
///
/// MERGE (track B → Phase 5): a collector no longer chooses recipients. It appends cited mail and
/// the ROUTER delivers, on the refs the item already carries — which is what `delivery_of`'s
/// "the item's OWN ref is always in `refs`" was always for. `dedupe_on` is the item's own ref, so
/// the at-least-once guard moves with the fan-out instead of being left behind in the collector.
pub fn envelope_of(item: &Collected, collector: &str) -> Envelope {
    let d = delivery_of(item, collector);
    Envelope {
        from: d.from,
        class: d.class,
        subject: d.subject,
        summary: d.summary,
        text: d.text,
        cites: d.cites,
        refs: d.refs,
        dedupe_on: Some(item.r#ref.clone()),
        at: d.at,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use bough_plugin_agents::MailClass;
    use bough_plugin_ledger::Ref;
    use chrono::{DateTime, Utc};

    use super::*;
    use crate::refs;

    fn at() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("a fixed instant")
    }

    fn item() -> Collected {
        Collected {
            r#ref: refs::pr("o/r", 12),
            url: Some("https://example.invalid/12".to_string()),
            subject: "PR #12".to_string(),
            summary: "a summary".to_string(),
            text: "a body".to_string(),
            refs: BTreeSet::from([Ref::new("lane:rebuild")]),
            class: MailClass::Wake,
            at: at(),
            order: 12,
        }
    }

    #[test]
    fn a_collected_item_is_cited_by_construction() {
        let d = delivery_of(&item(), "collector-github");
        assert_eq!(d.cites.len(), 1);
        assert_eq!(d.cites[0].r#ref, refs::pr("o/r", 12));
        assert_eq!(
            d.cites[0].url.as_deref(),
            Some("https://example.invalid/12")
        );
        assert_eq!(d.from, Sender::Collector("collector-github".to_string()));
        assert_eq!(d.class, MailClass::Wake);
        assert_eq!(d.at, at());
    }

    #[test]
    fn the_items_own_ref_is_always_in_refs_for_the_router() {
        let mut i = item();
        i.refs.clear();
        let d = delivery_of(&i, "collector-github");
        assert!(d.refs.contains(&refs::pr("o/r", 12)));
    }

    #[test]
    fn an_empty_url_is_no_url_rather_than_an_empty_one() {
        let mut i = item();
        i.url = Some("  ".to_string());
        assert_eq!(delivery_of(&i, "c").cites[0].url, None);
    }

    /// The router path carries the same bytes as the `deliver_to` path, plus the guard the router
    /// now holds. One builder, so the two destinations cannot drift.
    #[test]
    fn an_envelope_is_the_same_mail_plus_the_dedupe_key() {
        let i = item();
        let d = delivery_of(&i, "collector-github");
        let e = envelope_of(&i, "collector-github");
        assert_eq!(e.from, d.from);
        assert_eq!(e.class, d.class);
        assert_eq!(e.subject, d.subject);
        assert_eq!(e.text, d.text);
        assert_eq!(e.cites, d.cites);
        assert_eq!(e.refs, d.refs);
        assert_eq!(e.dedupe_on.as_ref(), Some(&i.r#ref));
    }

    #[test]
    fn delivery_of_is_pure() {
        let i = item();
        assert_eq!(delivery_of(&i, "c"), delivery_of(&i, "c"));
    }
}
