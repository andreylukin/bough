//! Invariant: THE SANDBOX IS CONSTRUCTED HERE AND NOWHERE ELSE, and its shape is code, not config
//! (P6-D10). `Engine::new_raw()` plus arithmetic/logic/string/array/map packages ONLY: no
//! filesystem, no process, no network, no `print`/`debug` sink beyond a captured string. `eval` is
//! DISABLED explicitly — rhai enables it by default, and §13 names this.
//!
//! Which limits are set is code (all five, plus the wall-clock bound `eval_timeout_ms`, always).
//! Their VALUES are config, bounded by `Plugin::validate`.

use std::cell::Cell;
use std::time::{Duration, Instant};

use rhai::packages::Package;

use crate::WardHostConfig;

thread_local! {
    /// Operations the LAST evaluation on this thread used. rhai counts them through `on_progress`,
    /// which is the only place the number exists; `ward/fired` records it, so a ward that is
    /// slowly growing is visible in the ledger rather than only in a timeout.
    static OPS: Cell<u64> = const { Cell::new(0) };

    /// When the evaluation running on this thread must stop. `on_progress` returning `Some`
    /// aborts the script with rhai's `ErrorTerminated`, which is what `WardError::Timeout` is
    /// made from — so `eval_timeout_ms` is a bound the engine actually enforces and not a
    /// documented promise. Checked every `TIME_CHECK_OPS` operations, because `Instant::now()`
    /// on every single one would cost more than the scripts do.
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

thread_local! {
    /// The budget [`start`] armed, in ms, so `WardError::Timeout` can NAME the bound it hit
    /// rather than reporting `0ms`.
    static BUDGET_MS: Cell<u64> = const { Cell::new(0) };
}

/// The budget the evaluation on this thread was given, in ms.
pub fn budget_ms() -> u64 {
    BUDGET_MS.with(|b| b.get())
}

/// How often the deadline is looked at, in rhai operations.
const TIME_CHECK_OPS: u64 = 1024;

/// Reset the counter before an evaluation.
pub fn reset_ops() {
    OPS.with(|o| o.set(0));
}

/// Operations the last evaluation on this thread used.
pub fn last_ops() -> u64 {
    OPS.with(|o| o.get())
}

/// Start the wall clock for the evaluation about to run on this thread. Called by [`reset_ops`]'s
/// caller through [`start`], never by the engine.
pub fn start(budget: Duration) {
    DEADLINE.with(|d| d.set(Some(Instant::now() + budget)));
    BUDGET_MS.with(|b| b.set(budget.as_millis() as u64));
    reset_ops();
}

/// Build the sandboxed engine. PURE, so the limits are testable without a tree.
pub fn build_engine(cfg: &WardHostConfig) -> rhai::Engine {
    let mut engine = rhai::Engine::new_raw();

    // The ONLY vocabulary a ward gets: arithmetic, logic, strings, arrays, maps. Every package
    // rhai ships that touches the outside world (files, time, process) is left out, so a ward
    // cannot spell I/O even by accident.
    engine.register_global_module(rhai::packages::ArithmeticPackage::new().as_shared_module());
    engine.register_global_module(rhai::packages::LogicPackage::new().as_shared_module());
    engine.register_global_module(rhai::packages::BasicStringPackage::new().as_shared_module());
    engine.register_global_module(rhai::packages::BasicArrayPackage::new().as_shared_module());
    engine.register_global_module(rhai::packages::BasicMapPackage::new().as_shared_module());

    // `eval` is rhai's own escape hatch out of a reviewed script and INTO an unreviewed one. §13
    // names it; disabling the symbol makes it a parse error rather than a runtime refusal.
    engine.disable_symbol("eval");

    // No module resolver at all: `import` cannot reach a file even if a ward spells it.
    engine.set_module_resolver(rhai::module_resolvers::DummyModuleResolver::new());

    // The five limits.
    engine.set_max_operations(cfg.max_ops);
    engine.set_max_expr_depths(cfg.max_depth, cfg.max_depth);
    engine.set_max_call_levels(cfg.max_depth);
    engine.set_max_string_size(cfg.max_string_bytes);
    engine.set_max_array_size(cfg.max_array_size);
    engine.set_max_map_size(cfg.max_array_size);

    engine.on_progress(|ops| {
        OPS.with(|o| o.set(ops));
        if ops % TIME_CHECK_OPS == 0
            && DEADLINE
                .with(|d| d.get())
                .is_some_and(|at| Instant::now() >= at)
        {
            // Any non-`None` token terminates the script; rhai reports `ErrorTerminated`.
            return Some(rhai::Dynamic::from(true));
        }
        None
    });
    engine
}
