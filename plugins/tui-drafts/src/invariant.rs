//! §0.2 runtime invariant for `bough-plugin-tui-drafts`:
//!
//! **Every draft row the pane rendered names a `draft/*` step the ledger still holds, and the
//! pane's key hints offer nothing that could send one.** The first half is the same "do not show
//! a fact that is no longer there" check a read-only pane owes; the second is §7's, checked at
//! runtime rather than trusted to a reading of `handle`.
//!
//! The recorder is a crate-level slot rather than a service binding, for `tui-search`'s reason:
//! there is exactly one terminal and therefore exactly one drafts pane, and `render` is
//! synchronous, so the check must not have to reach back into a pane that may be disposed.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_drafts::DraftRow;
use bough_plugin_ledger::{Ledger, StepId};
use parking_lot::Mutex;

const NAME: &str = "the_drafts_pane_shows_live_drafts_and_no_way_to_send_them";

/// Anything that would be a send. A hint carrying one of these is the failure this pane exists to
/// make impossible, so the words are checked rather than the wiring.
const SEND_WORDS: &[&str] = &["send", "post", "deliver", "publish", "submit", "reply"];

static LAST_RENDERED: Mutex<Vec<DraftRow>> = Mutex::new(Vec::new());

/// What the last frame put on screen. Called from `Pane::render`; allocation-only, no I/O.
pub fn record(rows: &[DraftRow]) {
    *LAST_RENDERED.lock() = rows.to_vec();
}

/// The rows the last frame painted.
pub fn rendered() -> Vec<DraftRow> {
    LAST_RENDERED.lock().clone()
}

/// Forget the recorded frame. The row's disposal path: a disabled row leaves nothing behind.
pub fn forget() {
    LAST_RENDERED.lock().clear();
}

/// PURE: no key hint offers a send.
pub fn check_hints(hints: &[(&str, &str)]) -> Result<(), String> {
    for (key, what) in hints {
        let lowered = what.to_lowercase();
        if let Some(word) = SEND_WORDS.iter().find(|w| lowered.contains(**w)) {
            return Err(format!(
                "the drafts pane offers `{key} {what}`, which reads as `{word}`: this pane has no \
                 send (§7)"
            ));
        }
    }
    Ok(())
}

/// PURE: every rendered row names a step the ledger still holds.
pub fn check_rows(rendered: &[DraftRow], known: &[StepId]) -> Result<(), String> {
    for row in rendered {
        if !known.contains(&row.step) {
            return Err(format!(
                "the drafts pane rendered draft `{}` (step `{}`), which the ledger no longer holds",
                row.id, row.step
            ));
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
        check: |ctx: Context| Box::pin(run(ctx)),
    }]
}

async fn run(ctx: Context) -> Result<(), InvariantViolation> {
    let fail = |detail: String| InvariantViolation {
        invariant: NAME,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    };
    check_hints(crate::KEY_HINTS).map_err(fail)?;
    let rows = rendered();
    if rows.is_empty() {
        return Ok(());
    }
    let Some(ledger) = ctx.peek_live::<Ledger>() else {
        // The row is being torn down; there is nothing to state about a ledger that is gone.
        return Ok(());
    };
    let mut known: Vec<StepId> = Vec::with_capacity(rows.len());
    for row in &rows {
        if ledger
            .0
            .step(&row.step)
            .await
            .map_err(|e| fail(e.to_string()))?
            .is_some()
        {
            known.push(row.step.clone());
        }
    }
    check_rows(&rows, &known).map_err(fail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_hints_offer_no_send() {
        check_hints(crate::KEY_HINTS).expect("the pane has no send");
    }

    #[test]
    fn a_send_hint_is_a_violation() {
        let err = check_hints(&[("s", "send to slack")]).expect_err("that is a send");
        assert!(err.contains("has no send"), "{err}");
    }

    #[test]
    fn a_reply_hint_is_a_violation_too() {
        assert!(check_hints(&[("r", "reply in thread")]).is_err());
    }
}
