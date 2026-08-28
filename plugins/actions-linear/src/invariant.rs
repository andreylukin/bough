//! §0.2 runtime invariant for `bough-plugin-actions-linear`:
//!
//! **No concluded `linear_write` created anything, and every one carries its own marker.** The
//! journal is the authority: a `linear_write` that reached `Done` has a result whose `marker` is
//! the one derived from its idem key, and whose detail names an ISSUE THAT ALREADY EXISTED — a
//! result mentioning a creation is the failure this looks for.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_actions::marker_for;
use bough_plugin_ledger::{ActionRow, ActionStatus, Ledger};

const NAME: &str = "linear_writes_create_nothing_and_carry_their_own_marker";

/// The whole invariant as a pure function of the journal's rows.
pub fn evaluate(rows: &[ActionRow]) -> Result<(), String> {
    for row in rows.iter().filter(|r| r.kind == "linear_write") {
        if row.status != ActionStatus::Done {
            continue;
        }
        let result = row.result.clone().unwrap_or(serde_json::Value::Null);
        let rendered = result.to_string();
        for creation in ["issueCreate", "created_issue", "create_ticket"] {
            if rendered.contains(creation) {
                return Err(format!(
                    "action `{}` concluded naming `{creation}`: a linear_write never creates",
                    row.id
                ));
            }
        }
        let want = marker_for(&row.idem_key);
        match result.get("marker").and_then(|m| m.as_str()) {
            Some(m) if m == want => {}
            Some(m) => {
                return Err(format!(
                    "action `{}` concluded carrying marker `{m}`, but its idem key names `{want}`",
                    row.id
                ))
            }
            None => {
                return Err(format!(
                    "action `{}` is done with no marker: the comment cannot be found again",
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

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::{ActionId, IdemKey, WakeId};
    use chrono::{TimeZone, Utc};

    fn row(status: ActionStatus, result: serde_json::Value) -> ActionRow {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        ActionRow {
            id: ActionId::new("a1"),
            wake: WakeId::new("w1"),
            idem_key: IdemKey::new("abcdef0123456789abcdef"),
            kind: "linear_write".into(),
            payload: serde_json::json!({}),
            status,
            result: Some(result),
            at,
            done_at: Some(at),
        }
    }

    fn good() -> String {
        marker_for(&IdemKey::new("abcdef0123456789abcdef"))
    }

    #[test]
    fn a_status_change_carrying_its_marker_is_clean() {
        assert_eq!(
            evaluate(&[row(
                ActionStatus::Done,
                serde_json::json!({ "marker": good(), "detail": { "status": "Done" } })
            )]),
            Ok(())
        );
    }

    #[test]
    fn a_result_naming_a_creation_is_reported() {
        assert!(evaluate(&[row(
            ActionStatus::Done,
            serde_json::json!({ "marker": good(), "detail": { "issueCreate": true } })
        )])
        .unwrap_err()
        .contains("never creates"));
    }

    #[test]
    fn a_done_with_no_marker_is_reported() {
        assert!(evaluate(&[row(ActionStatus::Done, serde_json::json!({}))])
            .unwrap_err()
            .contains("no marker"));
    }

    #[test]
    fn an_unconcluded_row_is_not_this_invariants_business() {
        assert_eq!(
            evaluate(&[row(ActionStatus::Intent, serde_json::json!({}))]),
            Ok(())
        );
    }
}
