//! Invariant: the filter is a CONJUNCTION over five dimensions, and within a dimension a
//! DISJUNCTION over its members (the `StepQuery` precedent). An empty field is "no filter", never
//! "match nothing". Time bounds are applied HERE and never pushed into `StepQuery` (decision D-C4).

use std::collections::BTreeSet;

use bough_plugin_ledger::{AgentName, Class, Ref, StepQuery, StepType, TrajId};
use chrono::{DateTime, Utc};

use crate::error::FilterError;
use crate::Row;

/// The composable filter. EVERY populated field is a CONJUNCT; an empty field is "no filter".
/// Within a field the members are a disjunction, so `agent:sol agent:terra type:tool/call` means
/// (sol ∨ terra) ∧ tool/call.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Filter {
    pub agents: BTreeSet<AgentName>,
    pub refs: BTreeSet<Ref>,
    pub kinds: BTreeSet<StepType>,
    pub class: Option<Class>,
    /// Inclusive.
    pub since: Option<DateTime<Utc>>,
    /// Exclusive.
    pub until: Option<DateTime<Utc>>,
}

impl Filter {
    /// PURE: the ∧ of the five dimensions.
    ///
    /// WP-2.
    pub fn matches(&self, row: &Row) -> bool {
        let _ = row;
        todo!("WP-2: conjoin the five dimensions")
    }

    /// Whether this filter constrains nothing.
    ///
    /// WP-2.
    pub fn is_empty(&self) -> bool {
        todo!("WP-2: every dimension unpopulated")
    }

    /// What the pane's header prints: `agent:sol ∧ ref:pr/1204 ∧ type:tool/call ∧ since:2h`.
    ///
    /// WP-2.
    pub fn describe(&self) -> String {
        todo!("WP-2: the header spelling of the filter")
    }

    /// The parts that can be pushed into a [`StepQuery`] — trajs, kinds and class. `since`/`until`
    /// are NOT pushed: `StepQuery` has no time bounds (decision D-C4), and pushing them would make
    /// `timeline()` a function of the store rather than of a slice.
    ///
    /// WP-2.
    pub fn to_query(&self, trajs: Vec<TrajId>, window: usize) -> StepQuery {
        let _ = (trajs, window);
        todo!("WP-2: trajs + kinds + class + limit(window), newest first")
    }
}

/// PURE: the filter grammar.
///
/// ```text
/// agent:sol ref:pr/1204 type:tool/call class:evidence since:2h until:2026-08-27T10:00:00Z
/// ```
///
/// `since`/`until` take an RFC3339 instant or a relative span (`15m`, `2h`, `3d`), resolved
/// against the `now` passed in — never against a clock read inside. An unknown word is an ERROR
/// naming the word (§16).
///
/// WP-2.
pub fn parse_filter(q: &str, now: DateTime<Utc>) -> Result<Filter, FilterError> {
    let _ = (q, now);
    todo!("WP-2: the filter grammar")
}

/// PURE: round-trips with [`parse_filter`] for every filter [`parse_filter`] can produce.
///
/// WP-2.
pub fn render_filter(f: &Filter, now: DateTime<Utc>) -> String {
    let _ = (f, now);
    todo!("WP-2: the round-tripping spelling")
}
