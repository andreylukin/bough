//! Invariant: there are exactly two entry classes (§3) — cited evidence and marked thought — and
//! evidence without citations is refused at append. A thought never promotes to evidence.

use std::collections::BTreeSet;
use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::id::{Ref, Seq, StepId, StepType, TrajId, WakeId};

/// §3's two entry classes. There is no third: control steps are [`Class::Thought`] (P1-D3).
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
    /// A truth claim. Requires at least one [`Cite`].
    Evidence,
    /// A marked thought. May carry cites, but is never rendered as a truth claim.
    Thought,
}

impl Class {
    /// The lowercase spelling stored in the `class` column and used in error messages.
    pub fn as_str(&self) -> &'static str {
        match self {
            Class::Evidence => "evidence",
            Class::Thought => "thought",
        }
    }
}

/// §3: `cites` is a JSON array of `{ref, url}`. Exactly that, no more.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize)]
pub struct Cite {
    /// The cited ref, in a scheme: `step:<id>`, `rollup:<id>`, `gh:o/r#12`, …
    #[serde(rename = "ref")]
    pub r#ref: Ref,
    /// An optional dereferenceable URL for the same fact.
    #[serde(default)]
    pub url: Option<String>,
}

/// What the caller asks to append.
///
/// `wake` and `at` are mandatory: wake_id is on every step (§3), and the clock is injected rather
/// than read inside the store (AGENTS.md).
#[derive(Clone, Debug)]
pub struct Append {
    pub traj: TrajId,
    pub wake: WakeId,
    pub kind: StepType,
    pub class: Class,
    pub body: serde_json::Value,
    pub cites: Vec<Cite>,
    pub at: DateTime<Utc>,
    /// `None` ⇒ the provider mints a uuid v7. Tests supply one so goldens are stable (P1-D6).
    pub id: Option<StepId>,
}

/// A committed row. Cheap to clone; the payload of `ledger/step`.
#[derive(Clone, Debug, PartialEq)]
pub struct Step {
    pub id: StepId,
    pub traj: TrajId,
    pub seq: Seq,
    pub at: DateTime<Utc>,
    pub wake: WakeId,
    pub kind: StepType,
    pub class: Class,
    pub body: Arc<serde_json::Value>,
    pub cites: Arc<Vec<Cite>>,
    /// CANONICAL for matching and routing (§3). Derived at append; never written by the caller.
    pub refs: Arc<BTreeSet<Ref>>,
    /// Copied from the step type's definition at append, so a binary that does not know the type
    /// can still decide whether to skip the row (P1-D7).
    pub ignorable: bool,
}

/// An inclusive run of seqs. The only compound scalar in the vocabulary; §5's consumed-set union
/// is a set of these.
#[derive(
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Debug,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct SeqRange {
    pub from: Seq,
    pub to: Seq,
}

impl SeqRange {
    /// Order-independent union: overlapping and adjacent ranges coalesce, the result is sorted.
    pub fn union(ranges: &[SeqRange]) -> Vec<SeqRange> {
        let mut sorted: Vec<SeqRange> = ranges.iter().copied().filter(|r| r.from <= r.to).collect();
        sorted.sort();
        let mut out: Vec<SeqRange> = Vec::with_capacity(sorted.len());
        for r in sorted {
            match out.last_mut() {
                // Overlapping OR adjacent (`to + 1 == from`) coalesce, so a union is a canonical
                // form and two different orders cannot produce two different answers.
                Some(last) if r.from.0 <= last.to.0.saturating_add(1) => {
                    if r.to > last.to {
                        last.to = r.to;
                    }
                }
                _ => out.push(r),
            }
        }
        out
    }
    /// Whether `seq` lies inside this range.
    pub fn contains(&self, seq: Seq) -> bool {
        self.from <= seq && seq <= self.to
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::id::Ref;

    fn r(from: u64, to: u64) -> SeqRange {
        SeqRange {
            from: Seq(from),
            to: Seq(to),
        }
    }

    /// The rule §3 states: anything rendered as truth is EVIDENCE and evidence carries citations.
    /// The refusal itself lives in [`crate::types::StepTypeMap::validate_append`]; this pins that
    /// the class + cites pair is what decides it, for the builtin `pin/set` (`ClassRule::Either`).
    #[test]
    fn evidence_without_cites_is_refused() {
        let map = crate::types::StepTypeMap::with_builtins();
        let req = crate::step::Append {
            traj: crate::id::TrajId::new("t"),
            wake: crate::id::WakeId::new("w"),
            kind: crate::id::StepType::new("pin/set"),
            class: Class::Evidence,
            body: serde_json::json!({ "title": "t", "text": "x" }),
            cites: vec![],
            at: chrono::Utc::now(),
            id: None,
        };
        let err = map
            .validate_append(&req)
            .expect_err("evidence with no cites must be refused");
        assert!(
            matches!(err, crate::LedgerError::EvidenceWithoutCites { .. }),
            "wrong refusal: {err}"
        );
    }

    /// A thought MAY cite; it simply never becomes evidence by doing so.
    #[test]
    fn thought_may_carry_cites() {
        let map = crate::types::StepTypeMap::with_builtins();
        let req = crate::step::Append {
            traj: crate::id::TrajId::new("t"),
            wake: crate::id::WakeId::new("w"),
            kind: crate::id::StepType::new("pin/set"),
            class: Class::Thought,
            body: serde_json::json!({ "title": "t", "text": "x" }),
            cites: vec![Cite {
                r#ref: Ref::new("gh:o/r#1"),
                url: None,
            }],
            at: chrono::Utc::now(),
            id: None,
        };
        let def = map.validate_append(&req).expect("a thought may cite");
        assert_eq!(def.name.as_str(), "pin/set");
    }

    #[test]
    fn seq_range_union_is_order_independent() {
        let a = vec![r(1, 3), r(4, 5), r(9, 9)];
        let b = vec![r(9, 9), r(4, 5), r(1, 3)];
        // 1-3 and 4-5 are adjacent, so they coalesce; 9 stands alone.
        assert_eq!(SeqRange::union(&a), vec![r(1, 5), r(9, 9)]);
        assert_eq!(SeqRange::union(&a), SeqRange::union(&b));
        // Overlap, containment and duplication all collapse to the same canonical form.
        assert_eq!(
            SeqRange::union(&[r(1, 10), r(3, 4), r(1, 10)]),
            vec![r(1, 10)]
        );
        assert_eq!(SeqRange::union(&[]), vec![]);
        assert!(r(1, 5).contains(Seq(5)));
        assert!(!r(1, 5).contains(Seq(6)));
    }
}
