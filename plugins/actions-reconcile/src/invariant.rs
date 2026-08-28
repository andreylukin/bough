//! §0.2 runtime invariant for `bough-plugin-actions-reconcile`:
//!
//! **A row reconciliation left open was left ALONE.** An action that is still `Intent` carries no
//! result and no done moment — in particular no `reconciled` result, which would mean a pass
//! concluded a row it also left open. And a row a pass DID conclude says so: its result carries
//! `reconciled: true` together with the artifact that was located.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{ActionRow, ActionStatus, Ledger};

const NAME: &str = "reconciliation_leaves_open_rows_untouched";

/// The whole invariant as a pure function of the journal's rows.
pub fn evaluate(rows: &[ActionRow]) -> Result<(), String> {
    for row in rows {
        match row.status {
            ActionStatus::Intent => {
                if row.result.is_some() {
                    return Err(format!(
                        "action `{}` is still `intent` but carries a result: a pass concluded a \
                         row it also left open",
                        row.id
                    ));
                }
            }
            ActionStatus::Done | ActionStatus::Failed => {
                let reconciled = row
                    .result
                    .as_ref()
                    .and_then(|r| r.get("reconciled"))
                    .and_then(|x| x.as_bool())
                    .unwrap_or(false);
                if reconciled
                    && row
                        .result
                        .as_ref()
                        .and_then(|r| r.get("locator"))
                        .and_then(|x| x.as_str())
                        .is_none()
                {
                    return Err(format!(
                        "action `{}` was reconciled without naming the artifact that was located",
                        row.id
                    ));
                }
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

    fn row(status: ActionStatus, result: Option<serde_json::Value>) -> ActionRow {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
        ActionRow {
            id: ActionId::new("a1"),
            wake: WakeId::new("w1"),
            idem_key: IdemKey::new("k"),
            kind: "open_pr".into(),
            payload: serde_json::json!({}),
            status,
            result,
            at,
            done_at: (status != ActionStatus::Intent).then_some(at),
        }
    }

    #[test]
    fn an_open_row_with_nothing_on_it_is_clean() {
        assert_eq!(evaluate(&[row(ActionStatus::Intent, None)]), Ok(()));
    }

    #[test]
    fn an_open_row_carrying_a_result_is_reported() {
        assert!(evaluate(&[row(
            ActionStatus::Intent,
            Some(serde_json::json!({"reconciled": true}))
        )])
        .unwrap_err()
        .contains("left open"));
    }

    #[test]
    fn a_reconciled_row_names_the_artifact_it_located() {
        assert_eq!(
            evaluate(&[row(
                ActionStatus::Done,
                Some(serde_json::json!({ "reconciled": true, "locator": "https://x" }))
            )]),
            Ok(())
        );
        assert!(evaluate(&[row(
            ActionStatus::Done,
            Some(serde_json::json!({ "reconciled": true }))
        )])
        .unwrap_err()
        .contains("without naming the artifact"));
    }
}
