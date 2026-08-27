//! Invariant: caps are enforced by the RUNTIME, not by the program. `set_memory_limit`,
//! `set_max_stack_size` and an interrupt handler that counts ops and samples the wall clock are
//! set before a single byte of the program's source is evaluated.

use std::sync::Arc;

use bough_plugin_js::{JsEngine, JsError, Program, Run};

use crate::QuickJsConfig;

/// The rquickjs engine. One `Runtime` per program, dropped after.
pub struct QuickJsEngine {
    #[allow(dead_code)]
    cfg: Arc<QuickJsConfig>,
    /// The barrier that enforces `max_concurrent_programs`.
    #[allow(dead_code)]
    slots: Arc<tokio::sync::Semaphore>,
}

impl QuickJsEngine {
    pub fn new(cfg: Arc<QuickJsConfig>) -> QuickJsEngine {
        let slots = Arc::new(tokio::sync::Semaphore::new(cfg.max_concurrent_programs));
        QuickJsEngine { cfg, slots }
    }
}

#[async_trait::async_trait]
impl JsEngine for QuickJsEngine {
    fn name(&self) -> &'static str {
        "quickjs"
    }

    /// Parse only, through the SAME engine that will run the program.
    ///
    /// WP-1 owns the body.
    async fn check(&self, _src: &str) -> Result<(), JsError> {
        todo!("WP-1: preflight::scan then a compile-only parse; map to JsError::Syntax")
    }

    /// Run the program wrapped in an async IIFE — `(async () => { <source> })()` — so top-level
    /// `await` works without any module machinery.
    ///
    /// WP-1 owns the body.
    async fn run(&self, _p: Program) -> Result<Run, JsError> {
        todo!("WP-1: one Runtime, caps set, host fns as promise-returning async closures")
    }
}
