//! Invariant: THE SANDBOX IS CONSTRUCTED HERE AND NOWHERE ELSE, and its shape is code, not config
//! (P6-D10). `Engine::new_raw()` plus arithmetic/logic/string/array/map packages ONLY: no
//! filesystem, no process, no network, no `print`/`debug` sink beyond a captured string. `eval` is
//! DISABLED explicitly — rhai enables it by default, and §13 names this.
//!
//! Which limits are set is code (all five, always). Their VALUES are config, bounded by
//! `Plugin::validate`.

use std::cell::Cell;

use rhai::packages::Package;

use crate::WardHostConfig;

thread_local! {
    /// Operations the LAST evaluation on this thread used. rhai counts them through `on_progress`,
    /// which is the only place the number exists; `ward/fired` records it, so a ward that is
    /// slowly growing is visible in the ledger rather than only in a timeout.
    static OPS: Cell<u64> = const { Cell::new(0) };
}

/// Reset the counter before an evaluation.
pub fn reset_ops() {
    OPS.with(|o| o.set(0));
}

/// Operations the last evaluation on this thread used.
pub fn last_ops() -> u64 {
    OPS.with(|o| o.get())
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
        None
    });
    engine
}
