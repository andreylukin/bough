//! Invariant: `ledger/step` is DURABLE (§0.2) — the row is already committed and readable when the
//! event fires. It is the ONE ledger event: `wake/*`, `step/*`, `pin/*`, `claim/*`, `action/*` and
//! the rest are STEP TYPES, and a consumer that wants "on wake start" filters the payload by
//! `kind`.

use std::sync::Arc;

use bough_kernel::EmitEvent;

use crate::step::Step;

/// `ledger/step`. Emitted POST-COMMIT, one per step, in seq order.
///
/// Emit mode, so an observer can neither fail nor delay the append.
pub struct LedgerStep;

impl EmitEvent for LedgerStep {
    const NAME: &'static str = "ledger/step";
    type Payload = Arc<Step>;
}
