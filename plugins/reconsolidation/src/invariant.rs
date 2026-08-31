//! §0.2 runtime invariant: **`a_pass_adds_and_never_edits`** — over the observed stream, every
//! step a pass appends is of a kind in `{claim/proposed, memory/expired, rollup/sealed,
//! about/line}`; and at quiesce, no `steps`/`edges` row hash observed before the first pass has
//! changed (§8: "never edits sealed rows or raw steps").
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{HashScope, Ledger, RowHash, StepId, StepType};
use parking_lot::Mutex;

/// The only kinds a pass may put into the ledger — its own two, plus the two the rollups seam
/// appends on its behalf when a distilled block is sealed.
pub const ADDABLE: &[&str] = &[
    "claim/proposed",
    crate::MEMORY_EXPIRED,
    "rollup/sealed",
    "about/line",
];

/// One pass, as observed.
#[derive(Clone, Debug)]
pub struct Obs {
    pub pass: crate::ReconPassId,
    /// Every step the pass appended, in append order.
    pub appended: Vec<(StepId, StepType)>,
    /// Row hashes read BEFORE the pass ran; the check re-reads them at quiesce.
    pub before: Vec<RowHash>,
}

/// What the row recorded this session, in pass order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one pass.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Everything recorded this session.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Forget the record. Tests only.
pub fn reset() {
    SEEN.lock().clear();
}

/// PURE: judge observed passes against the adds-and-never-edits statement, given the row hashes
/// as they stand now. Written as a function of data so a planted edit is a unit test.
///
/// A rollup's hash EXCLUDES `superseded_by` (the ledger's own rule), so the one write §3 permits
/// on a sealed row cannot be reported here as an edit — and any OTHER change to it can.
pub fn evaluate(obs: &[Obs], now: &[RowHash]) -> Result<(), String> {
    for o in obs {
        for (id, kind) in &o.appended {
            if !ADDABLE.contains(&kind.as_str()) {
                return Err(format!(
                    "pass `{}` appended step `{id}` of kind `{kind}`, which a reconsolidation \
                     pass may not write (§8: a pass adds and never edits)",
                    o.pass
                ));
            }
        }
    }
    for o in obs {
        for before in &o.before {
            match now
                .iter()
                .find(|r| r.table == before.table && r.id == before.id)
            {
                None => {
                    return Err(format!(
                        "row `{}` of `{}`, present before pass `{}`, is gone: a pass deletes \
                         nothing",
                        before.id, before.table, o.pass
                    ))
                }
                Some(after) if after.hash != before.hash => {
                    return Err(format!(
                        "row `{}` of `{}` changed across pass `{}`: a pass adds and never edits",
                        before.id, before.table, o.pass
                    ))
                }
                Some(_) => {}
            }
        }
    }
    Ok(())
}

/// §8: a pass adds and never edits.
pub fn a_pass_adds_and_never_edits() -> InvariantSpec {
    InvariantSpec {
        name: "a_pass_adds_and_never_edits",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: "a_pass_adds_and_never_edits",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let obs = seen();
    if obs.is_empty() {
        return Ok(());
    }
    // The ledger is a REQUIRED injection of this row, so its absence at quiesce is itself the
    // report rather than a silent pass.
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        return Err(fail(
            "a pass was observed but no `ledger` is bound to re-read its rows".into(),
        ));
    };
    let now = ledger
        .0
        .row_hashes(HashScope::All)
        .await
        .map_err(|e| fail(format!("row hashes are unreadable: {e}")))?;
    evaluate(&obs, &now).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass() -> crate::ReconPassId {
        crate::ReconPassId::new("p1")
    }

    fn hash(id: &str, h: &str) -> RowHash {
        RowHash {
            table: "steps",
            id: id.to_string(),
            hash: h.to_string(),
            superseded_by: None,
        }
    }

    fn clean() -> Obs {
        Obs {
            pass: pass(),
            appended: vec![
                (StepId::new("c1"), StepType::new("claim/proposed")),
                (StepId::new("e1"), StepType::new(crate::MEMORY_EXPIRED)),
            ],
            before: vec![hash("raw1", "aaa"), hash("raw2", "bbb")],
        }
    }

    #[test]
    fn a_clean_pass_passes() {
        let now = vec![
            hash("raw1", "aaa"),
            hash("raw2", "bbb"),
            // The two appends are new rows, which is exactly what adding looks like.
            hash("c1", "ccc"),
            hash("e1", "ddd"),
        ];
        evaluate(&[clean()], &now).expect("adding two of its own kinds is not an edit");
        // And a pass that observed nothing at all is vacuously clean.
        evaluate(&[], &now).expect("no pass, no violation");
    }

    #[test]
    fn a_planted_edit_is_reported() {
        // 1. A raw row rewritten under the pass.
        let edited = vec![hash("raw1", "CHANGED"), hash("raw2", "bbb")];
        let err = evaluate(&[clean()], &edited).expect_err("an edited raw row must be reported");
        assert!(err.contains("raw1") && err.contains("changed"), "{err}");

        // 2. A raw row deleted under the pass.
        let deleted = vec![hash("raw2", "bbb")];
        let err = evaluate(&[clean()], &deleted).expect_err("a deleted row must be reported");
        assert!(err.contains("raw1") && err.contains("gone"), "{err}");

        // 3. A kind a pass may not write.
        let mut forbidden = clean();
        forbidden
            .appended
            .push((StepId::new("x"), StepType::new("pin/set")));
        let err = evaluate(&[forbidden], &[hash("raw1", "aaa"), hash("raw2", "bbb")])
            .expect_err("a pass writing `pin/set` must be reported");
        assert!(err.contains("pin/set"), "{err}");
    }

    /// §3's one permitted write to a sealed row must not read as an edit: the ledger excludes
    /// `superseded_by` from a rollup's hash, and this pins that the check honours that.
    #[test]
    fn a_supersession_is_not_an_edit() {
        let before = Obs {
            pass: pass(),
            appended: vec![],
            before: vec![RowHash {
                table: "rollups",
                id: "tier:1".into(),
                hash: "aaa".into(),
                superseded_by: None,
            }],
        };
        let now = vec![RowHash {
            table: "rollups",
            id: "tier:1".into(),
            hash: "aaa".into(),
            superseded_by: Some("tier:1#1".into()),
        }];
        evaluate(&[before], &now).expect("set-once `superseded_by` is not a row edit");
    }
}
