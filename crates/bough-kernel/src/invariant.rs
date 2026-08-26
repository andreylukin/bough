//! Invariant: the invariant runner REPORTS and never acts. It collects the specs of every ACTIVE
//! fiber's plugin, runs them at their cadence, records violations and emits
//! `kernel/invariant-violated`. It never panics and never unloads anybody — a violation is a
//! report, so a false positive can never take the tree down (§0.2).
//!
//! It exists only when `KernelOptions::invariants` is true: the `dev` profile and the test
//! harness. In `tui` and `headless` it is not created at all.

use std::time::Duration;

use crate::context::Context;
use crate::fiber::EntryId;

/// One invariant a plugin crate owns, declared from its `src/invariant.rs`.
pub struct InvariantSpec {
    pub name: &'static str,
    pub plugin: &'static str,
    pub cadence: Cadence,
    pub check: fn(Context) -> futures::future::BoxFuture<'static, Result<(), InvariantViolation>>,
}

/// When an invariant runs.
#[derive(Clone, Copy, Debug)]
pub enum Cadence {
    /// Once, each time the tree quiesces.
    OnQuiesce,
    /// On a timer.
    Interval(Duration),
    /// Whenever the named event dispatches.
    OnEvent(&'static str),
}

/// A violation, as reported. Carries enough to act on without reading the check's source.
#[derive(Clone, Debug)]
pub struct InvariantViolation {
    pub invariant: &'static str,
    pub plugin: &'static str,
    pub entry: EntryId,
    pub detail: String,
}

/// The runner. Created by [`crate::Kernel`] iff `KernelOptions::invariants`.
pub struct InvariantRunner {
    _priv: (),
}

impl InvariantRunner {
    /// Collect specs from the ACTIVE fibers and start their cadences.
    pub fn start(ctx: Context, specs: Vec<InvariantSpec>) -> Self {
        todo!("WP-3")
    }
    /// Every violation recorded so far.
    pub fn violations(&self) -> Vec<InvariantViolation> {
        todo!("WP-3")
    }
    /// Run every `OnQuiesce` spec once. Called by the kernel after reconciliation settles.
    pub async fn run_on_quiesce(&self) {
        todo!("WP-3")
    }
}
