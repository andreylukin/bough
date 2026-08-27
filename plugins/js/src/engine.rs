//! Invariant: an engine owns NO I/O. Everything a program can reach arrives as a
//! [`crate::HostFn`], and the engine's only outputs are a [`crate::Run`] or a [`crate::JsError`].

use crate::{JsError, Program, Run};

/// What a JS runtime Provider implements. A second Provider is already named: main's sidecar
/// protocol (`crates/bough-core/src/harness/protocol.rs`), which is why this is a seam.
#[async_trait::async_trait]
pub trait JsEngine: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// Compile-only: parse, do not execute.
    async fn check(&self, src: &str) -> Result<(), JsError>;
    async fn run(&self, p: Program) -> Result<Run, JsError>;
}
