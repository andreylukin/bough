//! Invariant (P5-D8): the cited `graph/split` step is appended LAST. A crash mid-op leaves an
//! orphan trajectory and an edge nothing names — inert, invisible to `connected()` for any agent
//! without a row, and the op is simply re-runnable. Appending the op step first would leave a
//! cited FACT naming trajectories that do not exist, which is the failure mode §16 cares about.
//!
//! Order, identical for split, bud and fork: resolve the seq → plan → `ledger.fork` per child →
//! one inheritance digest per child through `ctx.rollups` → `put_agent` the children then the
//! reduced parent → append the cited step.

//! The body is WP-3's; the entry point is called from [`crate::GraphHandle`].
