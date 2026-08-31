//! Invariant (§8): the projector HONOURS the appended expiry marker, and honours it under `as_of`
//! exactly as every band does — a marker appended after the request being reproduced did not exist
//! for it (Phase 2 §2.7 item 3). Two bands deliberately do NOT honour expiry: `pins`, because a
//! pin's only relief valve is supersession (§3, V7), and `mail`, because unconsumed mail has its
//! own consumption mechanism and a marker must never silently un-deliver it.
//!
//! The SET itself is folded by `bough_plugin_rollups::expiry::parse` (P4-D7): the projector and the
//! governance rows read ONE implementation. This module owns only the READ — which rows are
//! markers, and which of them the request can see.

use bough_plugin_ledger::{Order, Step, StepQuery, StepType};
use bough_plugin_projection::{ProjectionError, SectionRequest};
use bough_plugin_rollups::Expired;

/// The one step type the projector reads as an expiry marker. The name is
/// `bough_plugin_rollups`' (§2.7: `reconsolidation` owns the WRITE, the seam owns the spelling);
/// re-exported here because the filter below is what a reader comes looking for.
pub use bough_plugin_rollups::expiry::EXPIRED_STEP_TYPE as MEMORY_EXPIRED;

/// The marker rows this request can see: every `memory/expired` step on a CONNECTED trajectory at
/// or below `as_of`, oldest first.
///
/// A marker is a step like any other, so `as_of` is the query's `before` bound rather than a
/// post-filter: the two spellings agree here, and the query is the one the tail band already uses.
pub(crate) async fn markers(req: &SectionRequest) -> Result<Vec<Step>, ProjectionError> {
    let steps = req
        .ledger
        .0
        .steps(&StepQuery {
            trajs: req.connected.trajectories().into_iter().collect(),
            kinds: vec![StepType::new(MEMORY_EXPIRED)],
            before: req.before(),
            order: Order::SeqAsc,
            ..Default::default()
        })
        .await?;
    Ok(steps)
}

/// Load the expiry set for one assembly.
///
/// Cheap when the governance rows are absent: with no `memory/expired` row on the trajectory the
/// query returns nothing and the set is empty, which is exactly what every band did before Phase 4.
pub async fn load(req: &SectionRequest) -> Result<Expired, ProjectionError> {
    let markers = markers(req).await?;
    if markers.is_empty() {
        // The common case, and it must not depend on the governance rows being loaded at all.
        return Ok(Expired::default());
    }
    Ok(bough_plugin_rollups::expiry::parse(&markers))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::*;
    use bough_plugin_ledger::Seq;

    /// The `as_of` half of `load`: which marker rows the request can SEE. Asserted on the read
    /// rather than on the folded set, because the fold is `bough-plugin-rollups`' to test (P4-D7)
    /// and this crate's claim is only that it hands it the right rows.
    #[tokio::test]
    async fn load_honours_as_of() {
        let f = Fixture::memory().await;
        f.seed_agent().await;
        let _tok = f.register_expiry_type();
        let early = f.expire(&["step:s1"], "superseded by a tier").await;
        let anchor = f.head().await;
        let _late = f.expire(&["step:s2"], "later still").await;

        let all = markers(&f.section_request(None)).await.expect("a read");
        assert_eq!(all.len(), 2, "unanchored, both markers are visible");

        let then = markers(&f.section_request(Some(anchor)))
            .await
            .expect("a read");
        assert_eq!(
            then.iter().map(|s| s.id.clone()).collect::<Vec<_>>(),
            vec![early],
            "a marker appended after `as_of` did not exist for the request being reproduced"
        );
    }

    /// The same rule stated from the other side: nothing above the anchor reaches the fold, so a
    /// marker written later can never retro-actively expire a row out of a replayed projection.
    #[tokio::test]
    async fn a_marker_above_as_of_is_invisible() {
        let f = Fixture::memory().await;
        f.seed_agent().await;
        let _tok = f.register_expiry_type();
        f.pin_set(
            "a row below the anchor",
            "so the trajectory has a head",
            &[],
        )
        .await;
        let anchor = f.head().await;
        f.expire(&["rollup:r-t1"], "the tier was wrong").await;

        assert!(
            markers(&f.section_request(Some(anchor)))
                .await
                .expect("a read")
                .is_empty(),
            "every marker sits above the anchor"
        );
        // …and `as_of` is INCLUSIVE of its own seq, so the marker is visible one seq later.
        let after = Seq(anchor.0 + 1);
        assert_eq!(
            markers(&f.section_request(Some(after)))
                .await
                .expect("a read")
                .len(),
            1
        );
    }
}
