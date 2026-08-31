//! §0.2 runtime invariant for `bough-plugin-js-quickjs`:
//!
//! **No `Runtime` outlives its program.** The count of live runtimes returns to zero after every
//! terminal outcome — a leaked runtime is a leaked heap and, worse, a program that could still
//! be running host functions after the seam reported it finished.

use bough_kernel::{Cadence, Context, InvariantSpec, InvariantViolation};
use std::sync::atomic::{AtomicI64, Ordering};

static LIVE: AtomicI64 = AtomicI64::new(0);

/// A `Runtime` was created.
pub fn opened() {
    LIVE.fetch_add(1, Ordering::SeqCst);
}

/// A `Runtime` was dropped.
pub fn closed() {
    LIVE.fetch_sub(1, Ordering::SeqCst);
}

/// How many runtimes are live right now.
pub fn live() -> i64 {
    LIVE.load(Ordering::SeqCst)
}

/// Test setup only.
pub fn clear() {
    LIVE.store(0, Ordering::SeqCst);
}

/// The whole invariant as a pure function of the live count.
pub fn evaluate(live: i64) -> Result<(), String> {
    match live {
        0 => Ok(()),
        n if n > 0 => Err(format!(
            "{n} QuickJS runtime(s) are still live after every program ended; a runtime must not \
             outlive its program"
        )),
        n => Err(format!(
            "the live runtime count is {n}: more runtimes were closed than were opened"
        )),
    }
}

/// The spec `QuickJsPlugin::invariants` returns.
pub fn no_runtime_outlives_its_program() -> InvariantSpec {
    InvariantSpec {
        name: "no_runtime_outlives_its_program",
        plugin: PLUGIN,
        cadence: Cadence::OnQuiesce,
        check: |ctx| Box::pin(check(ctx)),
    }
}

const PLUGIN: &str = crate::PLUGIN_NAME;

async fn check(ctx: Context) -> Result<(), InvariantViolation> {
    evaluate(live()).map_err(|detail| InvariantViolation {
        invariant: "no_runtime_outlives_its_program",
        plugin: PLUGIN,
        entry: ctx.entry_id().clone(),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_balanced_count_is_clean() {
        assert_eq!(evaluate(0), Ok(()));
    }

    #[test]
    fn a_leaked_runtime_is_a_violation() {
        let d = evaluate(2).expect_err("a leak must be reported");
        assert!(d.contains("still live"), "{d}");
    }

    #[test]
    fn a_negative_count_is_a_violation() {
        let d = evaluate(-1).expect_err("a double close must be reported");
        assert!(d.contains("more runtimes were closed"), "{d}");
    }
}
