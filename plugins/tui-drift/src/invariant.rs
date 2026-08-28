//! §0.2 statement for `bough-plugin-tui-drift`:
//!
//! **No runtime invariant: the pane owns no event stream and no data relation; every number it
//! renders is `drift-watch`'s, and `drift-watch` already checks them.** The two facts that could
//! go wrong here — the verdict never turning `TooFewSamples` into `Steady`, and the dispatched
//! line being exactly `/reset <agent>` — are pure and are pinned by unit tests
//! (`dash::tests::too_few_samples_is_its_own_verdict_not_steady`,
//! `pane::tests::the_reset_command_is_exactly_slash_reset_agent`), not by a runtime check.

use bough_kernel::InvariantSpec;

/// No specs, by the statement above.
pub fn specs() -> Vec<InvariantSpec> {
    Vec::new()
}
