//! §0.2 runtime invariant for `bough-plugin-actions-github`:
//!
//! **Every concluded act on GitHub carries its own name.** A row of one of this crate's three
//! kinds that reached `Done` has a result whose `marker` is exactly the marker DERIVED from that
//! row's idem key — which is what makes reconciliation a lookup rather than a guess. A `Done` with
//! a missing or foreign marker means something was written to the world that the journal cannot
//! find again, and that is the failure this checks for.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_actions::{marker_for, ActionKind};
use bough_plugin_ledger::{ActionRow, ActionStatus, Ledger};

const NAME: &str = "github_actions_conclude_with_their_own_marker";

/// The kinds this crate answers for.
fn mine(kind: &str) -> bool {
    matches!(kind, "open_pr" | "push_to_pr" | "bot_thread_op")
}

/// The whole invariant as a pure function of the journal's rows.
pub fn evaluate(rows: &[ActionRow]) -> Result<(), String> {
    for row in rows.iter().filter(|r| mine(&r.kind)) {
        if row.status != ActionStatus::Done {
            continue;
        }
        let want = marker_for(&row.idem_key);
        let got = row
            .result
            .as_ref()
            .and_then(|r| r.get("marker"))
            .and_then(|m| m.as_str());
        match got {
            Some(m) if m == want => {}
            Some(m) => {
                return Err(format!(
                    "action `{}` concluded carrying marker `{m}`, but its idem key names `{want}`",
                    row.id
                ))
            }
            None => {
                return Err(format!(
                    "action `{}` is done with no marker: the artifact cannot be found again",
                    row.id
                ))
            }
        }
    }
    Ok(())
}

/// The specs this crate contributes.
pub fn specs() -> Vec<InvariantSpec> {
    vec![InvariantSpec {
        name: NAME,
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }]
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let violation = |detail: String| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    let ledger = ctx.get::<Ledger>().map_err(|e| violation(e.to_string()))?;
    let rows = ledger
        .0
        .actions(&Default::default())
        .await
        .map_err(|e| violation(e.to_string()))?;
    evaluate(&rows).map_err(violation)
}

/// The kinds, for a reader of this module.
pub fn kinds() -> Vec<ActionKind> {
    vec![
        ActionKind::OpenPr,
        ActionKind::PushToPr,
        ActionKind::BotThreadOp,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{ActionId, IdemKey, WakeId};
    use chrono::{TimeZone, Utc};

    fn row(kind: &str, status: ActionStatus, marker: Option<&str>) -> ActionRow {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        ActionRow {
            id: ActionId::new("a1"),
            wake: WakeId::new("w1"),
            idem_key: IdemKey::new("abcdef0123456789abcdef"),
            kind: kind.into(),
            payload: serde_json::json!({}),
            status,
            result: marker.map(|m| serde_json::json!({ "marker": m })),
            at,
            done_at: (status != ActionStatus::Intent).then_some(at),
        }
    }

    fn good() -> String {
        marker_for(&IdemKey::new("abcdef0123456789abcdef"))
    }

    #[test]
    fn a_done_carrying_its_own_marker_is_clean() {
        assert_eq!(
            evaluate(&[row("open_pr", ActionStatus::Done, Some(&good()))]),
            Ok(())
        );
    }

    #[test]
    fn a_done_with_no_marker_is_reported() {
        assert!(evaluate(&[row("open_pr", ActionStatus::Done, None)])
            .unwrap_err()
            .contains("no marker"));
    }

    #[test]
    fn a_done_carrying_someone_elses_marker_is_reported() {
        assert!(evaluate(&[row(
            "push_to_pr",
            ActionStatus::Done,
            Some("bough-action:dead")
        )])
        .unwrap_err()
        .contains("its idem key names"));
    }

    #[test]
    fn an_unconcluded_row_and_another_crates_kind_are_not_this_invariants_business() {
        assert_eq!(
            evaluate(&[row("open_pr", ActionStatus::Intent, None)]),
            Ok(())
        );
        assert_eq!(
            evaluate(&[row("linear_write", ActionStatus::Done, None)]),
            Ok(())
        );
    }
}
