//! Invariant: the section ORDER is fixed by §5 and ties break by [`SectionId`], never by
//! registration order (P1-D8) — fiber activation order is not deterministic, so a golden built on
//! it would flake.

use std::sync::Arc;

use bough_plugin_ledger::{AgentName, Connected, LedgerHandle, RollupId, Seq, StepId, WakeId};
use chrono::{DateTime, Utc};

use crate::error::ProjectionError;

bough_util::brand_id!(
    /// The identity of one projection section. Also the tie-break key of the section order.
    pub struct SectionId;
);

/// The six fixed bands, in the order §5 fixes them.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    Identity,
    Pins,
    Digest,
    Tiers,
    Tail,
    Mail,
}

/// Where a section sits relative to its band.
///
/// DEVIATION from plan §2.7, which lists `Before` and `After` only: the built-in band itself needs
/// a place of its own, or `before_precedes_the_band_and_after_follows_it` cannot hold — a
/// `Before` section and the band would tie and break by [`SectionId`]. [`Place::Band`] is that
/// place, and the derived `Ord` (declaration order) is the order.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Place {
    Before,
    /// The built-in band itself.
    Band,
    After,
}

/// A contributed section's declared position.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Position {
    pub slot: Slot,
    pub place: Place,
}

impl Position {
    /// `(slot, place, id)`. Ties break by [`SectionId`], NEVER by registration order (P1-D8).
    pub fn sort_key<'a>(&self, id: &'a SectionId) -> (Slot, Place, &'a str) {
        (self.slot, self.place, id.as_str())
    }

    /// The position of a built-in band.
    pub fn band(slot: Slot) -> Position {
        Position {
            slot,
            place: Place::Band,
        }
    }
}

/// Which rung of the degradation ladder drops this section.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum DropPriority {
    /// Rung 1, with the fine tiers.
    Fine,
    /// Rung 3, with the remaining coarse tiers.
    Coarse,
    /// Never dropped: an answer wake must always be buildable (§5).
    Never,
}

/// Global, or scoped to one agent. An `Agent` spec SHADOWS a `Global` spec with the same
/// [`SectionId`], for that agent alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SectionScope {
    Global,
    Agent,
}

/// What `ctx.projection.section()` contributes.
pub struct SectionSpec {
    pub id: SectionId,
    pub position: Position,
    pub scope: SectionScope,
    /// `Some` iff `scope == SectionScope::Agent`.
    pub agent: Option<AgentName>,
    pub priority: DropPriority,
    pub render: Arc<dyn SectionRender>,
}

/// How a contributed section produces its text.
#[async_trait::async_trait]
pub trait SectionRender: Send + Sync + 'static {
    /// `Ok(None)` ⇒ the section contributes nothing this time and does not appear at all.
    async fn render(&self, req: &SectionRequest) -> Result<Option<SectionBody>, ProjectionError>;
}

/// What a section renderer is handed. `at` comes from the request: nothing in the request path
/// reads a clock.
#[derive(Clone, Debug)]
pub struct SectionRequest {
    pub agent: AgentName,
    pub wake: Option<WakeId>,
    pub at: DateTime<Utc>,
    pub ledger: LedgerHandle,
    pub connected: Arc<Connected>,
    /// The ledger high-water to render AT (§2.7 item 3). `None` ⇒ now.
    ///
    /// A contributed section that reads the ledger MUST honour it, or re-assembling a past
    /// request stops reproducing that request the moment anyone contributes a section.
    pub as_of: Option<Seq>,
}

impl SectionRequest {
    /// The `StepQuery::before` bound for this request: exclusive, so `as_of` itself is included.
    /// `None` when the request has no `as_of` and every committed row is visible.
    pub fn before(&self) -> Option<Seq> {
        self.as_of.map(|s| Seq(s.0 + 1))
    }

    /// Whether a row at `seq` is visible at this request's `as_of`.
    pub fn visible(&self, seq: Seq) -> bool {
        self.as_of.map(|a| seq <= a).unwrap_or(true)
    }
}

/// What a section renderer returns.
#[derive(Clone, Debug)]
pub struct SectionBody {
    pub title: String,
    pub body: String,
    pub cites: SectionCites,
}

/// Model-visible ⟺ ledgered (§0.2): every section says which ledger rows it renders from.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SectionCites {
    pub steps: Vec<StepId>,
    pub rollups: Vec<RollupId>,
}

impl SectionCites {
    /// The union of two cite sets, deduplicated and sorted. Used to finalize [`crate::Assembled`].
    pub fn union(&self, other: &SectionCites) -> SectionCites {
        let mut steps: Vec<StepId> = self
            .steps
            .iter()
            .chain(other.steps.iter())
            .cloned()
            .collect();
        steps.sort();
        steps.dedup();
        let mut rollups: Vec<RollupId> = self
            .rollups
            .iter()
            .chain(other.rollups.iter())
            .cloned()
            .collect();
        rollups.sort();
        rollups.dedup();
        SectionCites { steps, rollups }
    }
    /// Whether this section cites nothing at all.
    pub fn is_empty(&self) -> bool {
        self.steps.is_empty() && self.rollups.is_empty()
    }
}

/// Returned by [`crate::Projector::section`]; removes the section when disposed.
pub struct SectionToken {
    #[doc(hidden)]
    pub(crate) inner: Arc<dyn Fn() + Send + Sync>,
}

impl SectionToken {
    /// A token over a provider's own removal closure.
    ///
    /// DEVIATION from plan §2.7, which leaves the field `pub(crate)`: a provider lives in another
    /// crate, so the Definition must offer a constructor or no provider can return a token.
    pub fn new(remove: impl Fn() + Send + Sync + 'static) -> SectionToken {
        SectionToken {
            inner: Arc::new(remove),
        }
    }

    /// Remove the section from the registry.
    pub fn remove(self) {
        (self.inner)();
    }
}
