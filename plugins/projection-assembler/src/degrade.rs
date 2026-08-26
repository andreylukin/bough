//! Invariant: degradation runs in a FIXED reverse order and stops as soon as the draft fits, and
//! degradation of pins, digest or mail is NEVER SILENT — each raises an in-context flag (§5). The
//! ladder is a data-driven list, in one place, so it cannot drift into an `if` chain.
//!
//! | rung | action | flag |
//! |---|---|---|
//! | 1 | drop `tier` sections finest-first and every `DropPriority::Fine` section | — |
//! | 2 | shrink the tail toward `tail_floor_steps`, oldest first | — |
//! | 3 | drop remaining coarse tiers and `DropPriority::Coarse` sections | — |
//! | 4 | collapse pins to titles + count | `PinsDegraded` |
//! | 5 | collapse mail to per-class counts + newest N | `MailDegraded` |
//! | 6 | truncate the digest body to its first paragraph | `DigestDegraded` |
//! | — | still over | `OverBudget` — nothing is dropped silently |
//!
//! `DropPriority::Never` sections and the identity band are never dropped: an answer wake must
//! always be buildable (§5). Rung 3 exists so the ladder is TOTAL (P1-D21).
//!
//! DEVIATION from the plan's scaffold signature: a rung is
//! `fn(&mut Draft, &Cut) -> bool` rather than `fn(&mut Draft, &AssemblerConfig)`. A rung has to
//! know a contributed section's `DropPriority` and the rows a collapsed band was rendered from,
//! and `RenderedSection` (the Definition's type, §2.7) carries neither. [`Cut`] is that context;
//! the boolean is "this rung made progress", which is what lets rung 1 drop tiers FINEST-FIRST and
//! rung 2 shrink the tail ONE STEP AT A TIME while the ladder stays a flat list.

use std::collections::BTreeMap;

use bough_plugin_ledger::{Pin, Step};
use bough_plugin_projection::{tokens, Degradation, Draft, DropPriority, Flag, SectionId, Slot};

use crate::bands;
use crate::AssemblerConfig;

/// Everything a rung needs beyond the draft itself.
pub struct Cut {
    pub cfg: AssemblerConfig,
    /// Declared drop priority per contributed section. A section the waterfall added is not in
    /// here and is treated as [`DropPriority::Coarse`] — budgeted, and dropped no earlier than
    /// rung 3.
    pub priorities: BTreeMap<SectionId, DropPriority>,
    /// The tail window as it was selected, so rung 2 can re-render a shorter one.
    pub tail: Vec<Step>,
    /// The pins as `live_pins` returned them, so rung 4 can collapse them to titles.
    pub pins: Vec<Pin>,
    /// The unconsumed mail, so rung 5 can count it per class.
    pub mail: Vec<Step>,
    /// How many tail steps are currently rendered.
    tail_len: parking_lot::Mutex<usize>,
}

impl Cut {
    pub fn new(
        cfg: AssemblerConfig,
        priorities: BTreeMap<SectionId, DropPriority>,
        tail: Vec<Step>,
        pins: Vec<Pin>,
        mail: Vec<Step>,
    ) -> Cut {
        let n = tail.len();
        Cut {
            cfg,
            priorities,
            tail,
            pins,
            mail,
            tail_len: parking_lot::Mutex::new(n),
        }
    }

    fn priority(&self, id: &SectionId) -> DropPriority {
        self.priorities
            .get(id)
            .copied()
            .unwrap_or(DropPriority::Coarse)
    }
}

/// One rung of the ladder.
pub struct Rung {
    /// What the rung is called in a test failure.
    pub name: &'static str,
    /// Apply the rung to the draft in place. `true` ⇒ it made progress and may be applied again.
    pub apply: fn(&mut Draft, &Cut) -> bool,
}

/// The ladder, in order. THE readable statement of §5's degradation policy.
pub fn ladder() -> &'static [Rung] {
    &[
        Rung {
            name: "fine tiers and fine sections",
            apply: rung_fine,
        },
        Rung {
            name: "shrink the tail to its floor",
            apply: rung_tail,
        },
        Rung {
            name: "coarse tiers and coarse sections",
            apply: rung_coarse,
        },
        Rung {
            name: "collapse pins",
            apply: rung_pins,
        },
        Rung {
            name: "collapse mail",
            apply: rung_mail,
        },
        Rung {
            name: "truncate the digest",
            apply: rung_digest,
        },
    ]
}

/// The token size of a draft: exactly the text `Assembled::to_text` will print.
pub fn draft_tokens(draft: &Draft) -> usize {
    let body: usize = draft.sections.iter().map(|s| s.tokens).sum();
    body + flag_tokens(draft)
}

fn flag_tokens(draft: &Draft) -> usize {
    if draft.flags.is_empty() {
        0
    } else {
        tokens::count(&crate::assemble::flag_line(&draft.flags))
    }
}

/// Run the ladder until the draft fits `effective_budget`, or every rung is spent and
/// `OverBudget` is raised. Nothing is ever dropped silently.
pub fn degrade(draft: &mut Draft, cut: &Cut, effective_budget: usize) {
    if draft_tokens(draft) <= effective_budget {
        return;
    }
    for rung in ladder() {
        loop {
            if !(rung.apply)(draft, cut) {
                break;
            }
            if draft_tokens(draft) <= effective_budget {
                return;
            }
        }
        if draft_tokens(draft) <= effective_budget {
            return;
        }
    }
    draft.flags.insert(Flag::OverBudget);
}

/// A convenience for callers that hold the config rather than a `Cut`.
pub fn effective_budget(cfg: &AssemblerConfig, budget_tokens: usize) -> usize {
    tokens::effective_budget(budget_tokens, cfg.headroom)
}

// ---- the rungs --------------------------------------------------------------------------------

fn index_of(draft: &Draft, id: &str) -> Option<usize> {
    draft.sections.iter().position(|s| s.id.as_str() == id)
}

/// Drop the FINEST tier section present, or — when no tier is left — one `Fine` section.
///
/// §5 is "drop fine tiers first (KEEP COARSE), then shrink the verbatim tail": the coarsest tier
/// standing is not this rung's to take, or the tail would be cut only after every summary was
/// already gone. Rung 3 takes it, after the tail has reached its floor.
fn rung_fine(draft: &mut Draft, cut: &Cut) -> bool {
    if tier_count(draft) > 1 && drop_finest_tier(draft) {
        return true;
    }
    drop_priority(draft, cut, DropPriority::Fine)
}

fn tier_count(draft: &Draft) -> usize {
    draft
        .sections
        .iter()
        .filter(|s| bands::tier_of(&s.id).is_some())
        .count()
}

/// Drop a remaining tier (there are none by the time rung 3 runs, unless a listener added one) or
/// one `Coarse` section. Rung 3 is what makes the ladder TOTAL (P1-D21).
fn rung_coarse(draft: &mut Draft, cut: &Cut) -> bool {
    if drop_finest_tier(draft) {
        return true;
    }
    drop_priority(draft, cut, DropPriority::Coarse)
}

fn drop_finest_tier(draft: &mut Draft) -> bool {
    let finest = draft
        .sections
        .iter()
        .enumerate()
        .filter_map(|(i, s)| bands::tier_of(&s.id).map(|t| (t, i)))
        .min_by_key(|(t, _)| *t);
    match finest {
        Some((_, i)) => {
            draft.sections.remove(i);
            true
        }
        None => false,
    }
}

/// Drop one contributed section of `want`, highest id last so the order is deterministic.
/// Never touches a built-in band or a `Never` section.
fn drop_priority(draft: &mut Draft, cut: &Cut, want: DropPriority) -> bool {
    let victim = draft
        .sections
        .iter()
        .enumerate()
        .filter(|(_, s)| !is_builtin(&s.id))
        .filter(|(_, s)| cut.priority(&s.id) == want)
        .map(|(i, s)| (s.id.to_string(), i))
        .max();
    match victim {
        Some((_, i)) => {
            draft.sections.remove(i);
            true
        }
        None => false,
    }
}

/// The six built-in band ids. They are never dropped by a priority rung.
pub fn is_builtin(id: &SectionId) -> bool {
    matches!(
        id.as_str(),
        "identity" | "pins" | "digest" | "tail" | "mail"
    ) || bands::tier_of(id).is_some()
}

/// Shrink the tail by one step, oldest first, never below `tail_floor_steps`.
fn rung_tail(draft: &mut Draft, cut: &Cut) -> bool {
    let Some(i) = index_of(draft, "tail") else {
        return false;
    };
    let mut len = cut.tail_len.lock();
    if *len <= cut.cfg.tail_floor_steps || *len == 0 {
        return false;
    }
    *len -= 1;
    let window = &cut.tail[cut.tail.len() - *len..];
    match bands::tail_section(window) {
        Some(mut s) => {
            s.degraded = Some(Degradation::TailShrunk);
            draft.sections[i] = s;
        }
        None => {
            draft.sections.remove(i);
        }
    }
    true
}

/// Collapse pins to titles + count. Raises `PinsDegraded`: §5 forbids doing this silently.
fn rung_pins(draft: &mut Draft, cut: &Cut) -> bool {
    let Some(i) = index_of(draft, "pins") else {
        return false;
    };
    if draft.sections[i].degraded == Some(Degradation::PinsCollapsed) {
        return false;
    }
    draft.sections[i].body = bands::pins_collapsed_body(&cut.pins);
    draft.sections[i].degraded = Some(Degradation::PinsCollapsed);
    bands::remeasure(&mut draft.sections[i]);
    draft.flags.insert(Flag::PinsDegraded);
    true
}

/// Collapse mail to per-class counts + newest N. Raises `MailDegraded`.
fn rung_mail(draft: &mut Draft, cut: &Cut) -> bool {
    let Some(i) = index_of(draft, "mail") else {
        return false;
    };
    if draft.sections[i].degraded == Some(Degradation::MailCollapsed) {
        return false;
    }
    draft.sections[i].body = bands::mail_collapsed_body(&cut.mail, cut.cfg.mail_newest_n);
    draft.sections[i].degraded = Some(Degradation::MailCollapsed);
    bands::remeasure(&mut draft.sections[i]);
    draft.flags.insert(Flag::MailDegraded);
    true
}

/// Truncate the digest body to its first paragraph. Raises `DigestDegraded`.
fn rung_digest(draft: &mut Draft, cut: &Cut) -> bool {
    let _ = cut;
    let Some(i) = index_of(draft, "digest") else {
        return false;
    };
    if draft.sections[i].degraded == Some(Degradation::DigestTruncated) {
        return false;
    }
    draft.sections[i].body = bands::first_paragraph(&draft.sections[i].body);
    draft.sections[i].degraded = Some(Degradation::DigestTruncated);
    bands::remeasure(&mut draft.sections[i]);
    draft.flags.insert(Flag::DigestDegraded);
    true
}

/// `Slot` is re-exported here so a rung's author can name a band without importing the whole
/// Definition; the ladder itself never branches on it.
pub type Band = Slot;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bough_plugin_projection::{Place, Position, RenderedSection, SectionCites};

    fn draft(sections: Vec<RenderedSection>) -> Draft {
        Draft {
            request: std::sync::Arc::new(assemble_request("sol")),
            sections,
            budget: 0,
            flags: Default::default(),
        }
    }

    fn filler(id: &str, slot: Slot, words: usize) -> RenderedSection {
        let body = vec!["lorem ipsum dolor sit amet"; words].join(" ");
        RenderedSection {
            id: SectionId::new(id),
            position: Position {
                slot,
                place: Place::After,
            },
            title: id.to_string(),
            body,
            cites: SectionCites::default(),
            tokens: 0,
            degraded: None,
        }
    }

    fn measured(mut s: RenderedSection) -> RenderedSection {
        bands::remeasure(&mut s);
        s
    }

    fn cut(cfg: AssemblerConfig, prio: &[(&str, DropPriority)]) -> Cut {
        Cut::new(
            cfg,
            prio.iter()
                .map(|(id, p)| (SectionId::new(id), *p))
                .collect(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    fn ids(d: &Draft) -> Vec<String> {
        d.sections.iter().map(|s| s.id.to_string()).collect()
    }

    #[test]
    fn fine_tiers_go_first() {
        let mut d = draft(vec![
            measured(filler("identity", Slot::Identity, 1)),
            measured(filler(&bands::tier_section_id(3), Slot::Tiers, 40)),
            measured(filler(&bands::tier_section_id(1), Slot::Tiers, 40)),
        ]);
        let c = cut(cfg_small(), &[]);
        // A budget that fits identity plus one tier, but not two.
        let budget = d.sections[0].tokens + d.sections[1].tokens + 2;
        degrade(&mut d, &c, budget);
        assert_eq!(
            ids(&d),
            vec!["identity".to_string(), bands::tier_section_id(3)],
            "the FINEST tier (1) goes first, the coarse one stays"
        );
        assert!(d.flags.is_empty(), "dropping a tier raises no flag");
    }

    #[test]
    fn then_the_tail_shrinks_to_its_floor() {
        let steps: Vec<Step> = (1..=20).map(|n| step(&format!("s{n}"), n, "w1")).collect();
        let tail = bands::tail_section(&steps).unwrap();
        let mut d = draft(vec![measured(filler("identity", Slot::Identity, 1)), tail]);
        let mut cfg = cfg_small();
        cfg.tail_floor_steps = 10;
        let c = Cut::new(cfg, Default::default(), steps, Vec::new(), Vec::new());
        degrade(&mut d, &c, 1); // unsatisfiable: the rung runs to its floor
        let body = &d.sections[1].body;
        let kept = body.lines().filter(|l| l.starts_with("- #")).count();
        assert_eq!(kept, 10, "the tail stops at tail_floor_steps, never below");
        assert!(!body.contains("- #10 "), "the OLDEST steps went first");
        assert!(body.contains("- #20 "), "the newest step survives");
        assert_eq!(d.sections[1].degraded, Some(Degradation::TailShrunk));
    }

    #[test]
    fn pins_are_never_dropped_before_rung_four() {
        let pins = vec![pin(
            "p1",
            1,
            "a standing rule",
            "the long body of a standing rule",
        )];
        let mut d = draft(vec![
            bands::pins_section(&pins).unwrap(),
            measured(filler(&bands::tier_section_id(1), Slot::Tiers, 60)),
            measured(filler("extra", Slot::Tail, 60)),
        ]);
        let c = Cut::new(
            cfg_small(),
            [(SectionId::new("extra"), DropPriority::Fine)]
                .into_iter()
                .collect(),
            Vec::new(),
            pins,
            Vec::new(),
        );
        let budget = d.sections[0].tokens + 2;
        degrade(&mut d, &c, budget);
        assert_eq!(ids(&d), vec!["pins".to_string()], "pins are the survivor");
        assert!(
            !d.flags.contains(&Flag::PinsDegraded),
            "the ladder stopped before rung 4, so pins are intact and unflagged"
        );
        assert_eq!(d.sections[0].degraded, None);
    }

    #[test]
    fn a_collapsed_pin_set_raises_the_degraded_flag() {
        let pins = vec![
            pin(
                "p1",
                1,
                "rule one",
                "a very long body that costs real tokens to render",
            ),
            pin(
                "p2",
                2,
                "rule two",
                "another very long body that costs real tokens",
            ),
        ];
        let mut d = draft(vec![bands::pins_section(&pins).unwrap()]);
        let c = Cut::new(
            cfg_small(),
            Default::default(),
            Vec::new(),
            pins,
            Vec::new(),
        );
        degrade(&mut d, &c, 1);
        assert!(d.flags.contains(&Flag::PinsDegraded), "never silent (§5)");
        assert_eq!(d.sections[0].degraded, Some(Degradation::PinsCollapsed));
        let body = &d.sections[0].body;
        assert!(body.contains("2 pins"), "the count survives: {body}");
        assert!(
            body.contains("rule one") && body.contains("rule two"),
            "titles survive"
        );
        assert!(!body.contains("very long body"), "the text is what went");
    }

    #[test]
    fn a_collapsed_mail_header_keeps_per_class_counts_and_newest_n() {
        let mail: Vec<Step> = (1..=7)
            .map(|n| mail_step(&format!("m{n}"), n, "ordinary", &format!("subject {n}")))
            .collect();
        let mut d = draft(vec![bands::mail_section(&mail).unwrap()]);
        let mut cfg = cfg_small();
        cfg.mail_newest_n = 2;
        let c = Cut::new(cfg, Default::default(), Vec::new(), Vec::new(), mail);
        degrade(&mut d, &c, 1);
        assert!(d.flags.contains(&Flag::MailDegraded));
        let body = &d.sections[0].body;
        assert!(body.contains("### ordinary: 7 unconsumed"), "{body}");
        assert!(
            body.contains("subject 7") && body.contains("subject 6"),
            "newest 2 kept"
        );
        assert!(
            !body.contains("subject 1"),
            "the rest are counted, not printed"
        );
        assert!(body.contains("… 5 older, collapsed"), "{body}");
    }

    #[test]
    fn over_budget_after_every_rung_raises_over_budget_and_drops_nothing_silently() {
        let pins = vec![pin("p1", 1, "rule", "body")];
        let mut d = draft(vec![
            measured(filler("identity", Slot::Identity, 40)),
            bands::pins_section(&pins).unwrap(),
        ]);
        let c = Cut::new(
            cfg_small(),
            Default::default(),
            Vec::new(),
            pins,
            Vec::new(),
        );
        degrade(&mut d, &c, 1);
        assert!(d.flags.contains(&Flag::OverBudget));
        assert_eq!(
            ids(&d),
            vec!["identity".to_string(), "pins".to_string()],
            "identity and pins are still there: an answer wake must stay buildable"
        );
        assert!(
            d.flags.contains(&Flag::PinsDegraded),
            "and the collapse was announced"
        );
    }

    #[test]
    fn a_never_priority_section_survives_every_rung() {
        let mut d = draft(vec![
            measured(filler("identity", Slot::Identity, 1)),
            measured(filler("standing-order", Slot::Tail, 80)),
            measured(filler("chatter", Slot::Tail, 80)),
        ]);
        let c = cut(
            cfg_small(),
            &[
                ("standing-order", DropPriority::Never),
                ("chatter", DropPriority::Fine),
            ],
        );
        degrade(&mut d, &c, 1);
        assert!(
            ids(&d).contains(&"standing-order".to_string()),
            "a Never section is never dropped: {:?}",
            ids(&d)
        );
        assert!(!ids(&d).contains(&"chatter".to_string()));
        assert!(d.flags.contains(&Flag::OverBudget));
    }
}
