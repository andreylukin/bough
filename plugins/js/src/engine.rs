//! Invariant: an engine owns NO I/O. Everything a program can reach arrives as a
//! [`crate::HostFn`], and the engine's only outputs are a [`crate::Run`] or a [`crate::JsError`].

use crate::{Caps, JsError, Program, Run};

/// What a JS runtime Provider implements. A second Provider is already named: main's sidecar
/// protocol (`crates/bough-core/src/harness/protocol.rs`), which is why this is a seam.
#[async_trait::async_trait]
pub trait JsEngine: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    /// Compile-only: parse, do not execute. The preflight runs under the SAME `Caps` the run
    /// would get (§0.2): the envelope is config, never a constant inside an engine.
    async fn check(&self, src: &str, caps: Caps) -> Result<(), JsError>;

    /// The same parse, told which names the sandbox will inject.
    ///
    /// A program that declares `let bash = 1` is a SyntaxError whose useful message names the
    /// shadowed host function — and that message can only be written by a parser that knows the
    /// bound names. A caller that preflights (`JsHandle::check`) and returns on the error never
    /// reaches the run path where the names are known, so the diagnostic has to be reachable
    /// HERE. The default ignores `bound`, so an engine that has no such diagnostic is unaffected.
    async fn check_bound(&self, src: &str, caps: Caps, _bound: &[String]) -> Result<(), JsError> {
        self.check(src, caps).await
    }
    async fn run(&self, p: Program) -> Result<Run, JsError>;
}
