//! Invariant: THE SANDBOX IS CONSTRUCTED HERE AND NOWHERE ELSE, and its shape is code, not config
//! (P6-D10). `Engine::new_raw()` plus arithmetic/array/map packages ONLY: no filesystem, no
//! process, no network, no `print`/`debug` sink beyond a captured string. `eval` is DISABLED
//! explicitly — rhai enables it by default, and §13 names this.

use crate::WardHostConfig;

/// Build the sandboxed engine. PURE, so the limits are testable without a tree. WP-6.
pub fn build_engine(cfg: &WardHostConfig) -> rhai::Engine {
    let _ = cfg;
    todo!("WP-6: new_raw + packages, disable_symbol(\"eval\"), all five limits set")
}
