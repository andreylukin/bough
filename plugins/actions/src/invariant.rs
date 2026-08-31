//! §0.2 runtime invariant for `bough-plugin-actions`:
//!
//! **Every journal row has its intent written before its done, and no two rows share an idem
//! key.**
//!
//! Unlike the other invariants in this phase this one reads the `actions` TABLE at quiesce rather
//! than an event stream: the relation it is about is a data relation, and the table is the
//! authority on it.

use std::collections::BTreeMap;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::{ActionRow, ActionStatus, Ledger};

/// The whole invariant as a pure function of the journal's rows.
///
/// "Intent before done" is checked as a shape, because that is all a row can show after the fact:
/// a concluded row carries a `done_at` at or after its `at`, and an unconcluded one carries none.
/// A row that is `done` with no `done_at` is a done written without an intent moment.
pub fn evaluate(rows: &[ActionRow]) -> Result<(), String> {
    let mut seen: BTreeMap<&str, &ActionRow> = BTreeMap::new();
    for row in rows {
        match row.status {
            ActionStatus::Intent => {
                if let Some(done) = row.done_at {
                    return Err(format!(
                        "action `{}` is still `intent` but carries a done at {done}",
                        row.id
                    ));
                }
            }
            ActionStatus::Done | ActionStatus::Failed => match row.done_at {
                None => {
                    return Err(format!(
                    "action `{}` is `{}` with no done moment: a done without an intent before it",
                    row.id,
                    status_str(row.status)
                ))
                }
                Some(done) if done < row.at => {
                    return Err(format!(
                        "action `{}` was done at {done} before its intent at {}",
                        row.id, row.at
                    ))
                }
                Some(_) => {}
            },
        }
        if let Some(other) = seen.insert(row.idem_key.as_str(), row) {
            return Err(format!(
                "actions `{}` and `{}` share idem key `{}`: one act was journalled twice",
                other.id, row.id, row.idem_key
            ));
        }
    }
    Ok(())
}

fn status_str(s: ActionStatus) -> &'static str {
    match s {
        ActionStatus::Intent => "intent",
        ActionStatus::Done => "done",
        ActionStatus::Failed => "failed",
    }
}

/// The spec `ActionsPlugin::invariants` returns.
pub fn journal_is_intent_before_done() -> InvariantSpec {
    InvariantSpec {
        name: "action_journal_is_intent_before_done_with_unique_idem_keys",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    let violation = |detail: String| InvariantViolation {
        invariant: "action_journal_is_intent_before_done_with_unique_idem_keys",
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

    fn row(id: &str, key: &str, status: ActionStatus, done_offset: Option<i64>) -> ActionRow {
        let at = Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 10).unwrap();
        ActionRow {
            id: ActionId::new(id),
            wake: WakeId::new("w1"),
            idem_key: IdemKey::new(key),
            kind: "open_pr".into(),
            payload: serde_json::json!({}),
            status,
            result: None,
            at,
            done_at: done_offset.map(|s| at + chrono::Duration::seconds(s)),
        }
    }

    #[test]
    fn a_journal_of_well_formed_rows_is_clean() {
        let rows = vec![
            row("a1", "k1", ActionStatus::Intent, None),
            row("a2", "k2", ActionStatus::Done, Some(1)),
            row("a3", "k3", ActionStatus::Failed, Some(0)),
        ];
        assert_eq!(evaluate(&rows), Ok(()));
    }

    #[test]
    fn a_done_with_no_done_moment_is_reported() {
        let rows = vec![row("a1", "k1", ActionStatus::Done, None)];
        assert!(evaluate(&rows).unwrap_err().contains("no done moment"));
    }

    #[test]
    fn a_done_before_its_intent_is_reported() {
        let rows = vec![row("a1", "k1", ActionStatus::Done, Some(-5))];
        assert!(evaluate(&rows).unwrap_err().contains("before its intent"));
    }

    #[test]
    fn two_rows_sharing_an_idem_key_are_reported() {
        let rows = vec![
            row("a1", "k1", ActionStatus::Done, Some(1)),
            row("a2", "k1", ActionStatus::Done, Some(1)),
        ];
        assert!(evaluate(&rows).unwrap_err().contains("share idem key"));
    }
}
