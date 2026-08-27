//! Invariant: `run` is an ORDINARY tool. It is registered through `ToolsHandle::register` and
//! guarded by the same pipeline as any other, and every call it makes from inside the sandbox
//! goes through that same pipeline too — there is no back door around the seam.

use std::sync::Arc;

use bough_plugin_tools::{Tool, ToolCall, ToolCx, ToolFailure, ToolOutcome, ToolSpec};

use crate::CodemodeConfig;

/// The ONE API tool.
pub struct Run {
    #[allow(dead_code)]
    pub cfg: Arc<CodemodeConfig>,
}

/// The deterministic id of the `n`-th inner call of the program `run` call `program`.
/// Deterministic ids are what make a replayed program reproduce the ledger it recorded.
pub fn inner_call_id(program: &str, n: u32) -> String {
    format!("{program}.{n}")
}

#[async_trait::async_trait]
impl Tool for Run {
    /// Always exclusive: a program is a barrier by construction.
    fn is_concurrency_safe(&self, _args: &serde_json::Value) -> bool {
        false
    }

    /// 1. preflight `js.check` (syntax lands as `program/error` + a failed result);
    /// 2. snapshot the agent's tools and build the mirror;
    /// 3. build the `HostFn`s (aliases, namespaces, the read/write concurrency lock);
    /// 4. `js.run`;
    /// 5. map the single terminal outcome.
    ///
    /// WP-2 owns the body.
    async fn call(&self, _call: Arc<ToolCall>, _cx: ToolCx) -> Result<ToolOutcome, ToolFailure> {
        todo!("WP-2: preflight → mirror → bind → js.run → terminal outcome")
    }
}

/// The single spec this row registers. `RenderIntent::Generic`, `ToolScope::Global`, and an
/// input schema of exactly one string field: there are NO per-request schemas under code mode —
/// the surface is one projection section.
///
/// WP-2 owns the body.
pub fn spec(_cfg: Arc<CodemodeConfig>) -> ToolSpec {
    todo!("WP-2: the `run` ToolSpec")
}
