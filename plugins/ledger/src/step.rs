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
        todo!("WP-1: Class::as_str")
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
        todo!("WP-1: SeqRange::union")
    }
    /// Whether `seq` lies inside this range.
    pub fn contains(&self, seq: Seq) -> bool {
        todo!("WP-1: SeqRange::contains")
    }
}
