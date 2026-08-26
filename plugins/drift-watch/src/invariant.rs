//! §0.2 runtime invariant: **`a_reset_rebuilds_and_never_reseals`** — for every `drift/reset`
//! observed, the `about/line` it names has an EMPTY intent half, and the count of `rollup/sealed`
//! observations of kind `tier` is unchanged across the reset (§8).
//!
//! Cadence is [`bough_kernel::Cadence::OnQuiesce`] (P1-D14).

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::StepId;
use parking_lot::Mutex;

/// One reset, as observed.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub reset_step: StepId,
    pub about_line: StepId,
    /// The intent half of the about-line the reset appended. Must be empty.
    pub intent: String,
    /// Sealed `tier` rollups on the trajectory before and after. Must be equal.
    pub tiers_before: usize,
    pub tiers_after: usize,
}

/// What the row recorded this session, in reset order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one reset.
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

/// PURE: judge observed resets. Written as a function of data so a planted violation is a unit
/// test rather than a live run.
pub fn evaluate(obs: &[Obs]) -> Result<(), String> {
    for o in obs {
        // §8: the STATE half is rebuilt from raw evidence and the INTENT half starts empty. An
        // intent carried across a reset is exactly the drift the reset exists to undo.
        if !o.intent.trim().is_empty() {
            return Err(format!(
                "reset `{}` appended about-line `{}` with a non-empty intent half ({:?}): a reset \
                 rebuilds the state half and leaves intent EMPTY",
                o.reset_step, o.about_line, o.intent
            ));
        }
        // §8: sealed tiers are read, never re-summarized and never re-sealed.
        if o.tiers_before != o.tiers_after {
            return Err(format!(
                "reset `{}` changed the sealed tier count from {} to {}: a reset never writes a \
                 sealed row",
                o.reset_step, o.tiers_before, o.tiers_after
            ));
        }
    }
    Ok(())
}

/// §8: a reset rebuilds and never reseals.
pub fn a_reset_rebuilds_and_never_reseals() -> InvariantSpec {
    InvariantSpec {
        name: "a_reset_rebuilds_and_never_reseals",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(&seen()).map_err(|detail| InvariantViolation {
        invariant: "a_reset_rebuilds_and_never_reseals",
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean() -> Obs {
        Obs {
            reset_step: StepId::new("r1"),
            about_line: StepId::new("a1"),
            intent: String::new(),
            tiers_before: 3,
            tiers_after: 3,
        }
    }

    #[test]
    fn a_clean_reset_passes() {
        evaluate(&[clean()]).expect("a reset that rebuilt and resealed nothing is legal");
        evaluate(&[]).expect("nothing observed is nothing to report");
        // Whitespace is not an intent.
        let mut blank = clean();
        blank.intent = "   \n".to_string();
        evaluate(&[blank]).expect("a whitespace-only intent half is empty");
    }

    #[test]
    fn a_reset_that_reseals_a_tier_is_reported() {
        let mut bad = clean();
        bad.tiers_after = 4;
        let err = evaluate(&[bad]).expect_err("a reset that sealed a tier is a violation");
        assert!(err.contains("sealed tier count"), "{err}");
        assert!(err.contains("3 to 4"), "{err}");

        // Losing one is a violation too: a reset never REMOVES a sealed row either.
        let mut lost = clean();
        lost.tiers_after = 2;
        assert!(evaluate(&[lost]).is_err());
    }

    #[test]
    fn a_reset_with_a_non_empty_intent_is_reported() {
        let mut bad = clean();
        bad.intent = "keep doing what I was doing".to_string();
        let err = evaluate(&[bad]).expect_err("a carried-over intent half is a violation");
        assert!(err.contains("intent"), "{err}");
        assert!(err.contains("EMPTY"), "{err}");

        // One bad observation among good ones is still reported.
        let mut bad = clean();
        bad.intent = "x".to_string();
        assert!(evaluate(&[clean(), bad, clean()]).is_err());
    }
}
