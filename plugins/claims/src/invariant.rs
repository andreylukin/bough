//! §0.2 runtime invariants for the claims seam, both statements about the DECISION STREAM this
//! row wrote rather than about what the seam remembers of itself.
//!
//! 1. **`decided_once`** — no claim has both an accepted and a rejected step, and none has two of
//!    either. A claim is decided once.
//! 2. **`accepted_requirement_has_a_pin`** — every accepted `Requirement` produced a `pin/set`.
//!    §3's "accepted requirements are pins" in its durable form.
//!
//! Cadence [`bough_kernel::Cadence::OnQuiesce`] for both (P1-D14).

use std::collections::BTreeMap;

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use bough_plugin_ledger::StepId;
use parking_lot::Mutex;

use crate::ClaimId;

/// One decision, as observed.
#[derive(Clone, Debug, PartialEq)]
pub struct Obs {
    pub claim: ClaimId,
    pub accepted: bool,
    /// `true` iff the claim's kind was a `Requirement`.
    pub requirement: bool,
    /// The `pin/set` the acceptance produced, if any.
    pub pin: Option<StepId>,
}

/// What the row recorded this session, in decision order.
static SEEN: Mutex<Vec<Obs>> = Mutex::new(Vec::new());

/// Record one decision.
pub fn record(obs: Obs) {
    SEEN.lock().push(obs);
}

/// Everything recorded this session.
pub fn seen() -> Vec<Obs> {
    SEEN.lock().clone()
}

/// Forget the record. Tests, and the fiber-life inverse.
pub fn reset() {
    SEEN.lock().clear();
}

/// PURE: clause 1.
pub fn evaluate_decided_once(obs: &[Obs]) -> Result<(), String> {
    let mut by_claim: BTreeMap<&ClaimId, Vec<bool>> = BTreeMap::new();
    for o in obs {
        by_claim.entry(&o.claim).or_default().push(o.accepted);
    }
    for (claim, decisions) in by_claim {
        if decisions.len() > 1 {
            let accepted = decisions.iter().filter(|d| **d).count();
            return Err(format!(
                "claim `{claim}` was decided {} times ({accepted} accepted, {} rejected): a claim \
                 is decided once",
                decisions.len(),
                decisions.len() - accepted
            ));
        }
    }
    Ok(())
}

/// PURE: clause 2.
pub fn evaluate_requirement_pins(obs: &[Obs]) -> Result<(), String> {
    for o in obs {
        if o.accepted && o.requirement && o.pin.is_none() {
            return Err(format!(
                "claim `{}` was accepted as a requirement and set no pin: an accepted requirement \
                 IS a pin (§3)",
                o.claim
            ));
        }
    }
    Ok(())
}

/// Clause 1.
pub fn decided_once() -> InvariantSpec {
    InvariantSpec {
        name: "decided_once",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx, "decided_once", evaluate_decided_once)),
    }
}

/// Clause 2.
pub fn accepted_requirement_has_a_pin() -> InvariantSpec {
    InvariantSpec {
        name: "accepted_requirement_has_a_pin",
        plugin: crate::PLUGIN_NAME,
        cadence: Cadence::OnQuiesce,
        check: |ctx| {
            Box::pin(check(
                ctx,
                "accepted_requirement_has_a_pin",
                evaluate_requirement_pins,
            ))
        },
    }
}

async fn check(
    ctx: Context,
    name: &'static str,
    f: fn(&[Obs]) -> Result<(), String>,
) -> Result<(), InvariantViolation> {
    f(&seen()).map_err(|detail| InvariantViolation {
        invariant: name,
        plugin: crate::PLUGIN_NAME,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted_requirement() -> Obs {
        Obs {
            claim: ClaimId::new("c1"),
            accepted: true,
            requirement: true,
            pin: Some(StepId::new("p1")),
        }
    }

    #[test]
    fn a_claim_decided_once_passes_and_twice_is_reported() {
        evaluate_decided_once(&[accepted_requirement()]).expect("one decision is legal");
        let twice = vec![
            accepted_requirement(),
            Obs {
                accepted: false,
                pin: None,
                ..accepted_requirement()
            },
        ];
        let err = evaluate_decided_once(&twice).expect_err("two decisions on one claim");
        assert!(err.contains("decided 2 times"), "{err}");
    }

    #[test]
    fn an_accepted_requirement_without_a_pin_is_reported() {
        evaluate_requirement_pins(&[accepted_requirement()]).expect("a pinned requirement is fine");
        // A rejected requirement pins nothing, and that is not a violation.
        evaluate_requirement_pins(&[Obs {
            accepted: false,
            requirement: true,
            pin: None,
            ..accepted_requirement()
        }])
        .expect("a rejection sets no pin");
        let err = evaluate_requirement_pins(&[Obs {
            pin: None,
            ..accepted_requirement()
        }])
        .expect_err("an accepted requirement with no pin");
        assert!(err.contains("set no pin"), "{err}");
    }
}
