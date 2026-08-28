//! Invariant: a `Program` is a CLOSED world. Its source, its caps, its host functions and its
//! console sink are all the engine gets; there is no ambient capability an engine may add.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;

/// One program, and everything it can reach.
pub struct Program {
    pub source: String,
    pub caps: Caps,
    /// Injected as globals in NAME order. A dotted name builds a namespace object
    /// (`ledger.search`), and a name that is both a function and a namespace root
    /// (`bg`, `bg.output`) becomes a callable object.
    pub host: Vec<HostFn>,
    pub console: Arc<dyn ConsoleSink>,
    pub cancel: CancellationToken,
}

/// One injected global.
pub struct HostFn {
    pub name: String,
    pub arity: u8,
    pub body: Arc<dyn HostCall>,
}

/// The host side of one injected global.
#[async_trait::async_trait]
pub trait HostCall: Send + Sync + 'static {
    /// `Ok` resolves the promise; `Err` rejects it with a JS `Error` carrying `kind`.
    async fn call(&self, args: Vec<serde_json::Value>) -> Result<serde_json::Value, HostRefusal>;
}

/// Why a host call rejected. Mirrors the tools seam's `FailureClass` without depending on it:
/// the `js` seam has no domain vocabulary.
pub struct HostRefusal {
    pub kind: RefusalKind,
    pub message: String,
}

/// The rejection taxonomy a program sees on `err.kind`.
#[derive(
    Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum RefusalKind {
    NotFound,
    Denied,
    Blocked,
    Timeout,
    Cancelled,
    Error,
}

/// Where `console.log(...)` goes.
pub trait ConsoleSink: Send + Sync + 'static {
    /// One already-formatted line. Called on the engine's thread; the sink MUST NOT block.
    /// `tools-codemode`'s sink buffers and flushes `program/console` steps.
    fn write(&self, line: &str);
}

/// The resource envelope. Deployment-varying, so it comes from `JsConfig` and never from a
/// constant in an engine.
#[derive(
    Clone, Copy, PartialEq, Debug, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Caps {
    /// Interrupt-handler budget.
    pub ops: u64,
    pub memory_bytes: usize,
    pub stack_bytes: usize,
    pub wall_ms: u64,
    pub console_bytes: usize,
}

/// What a program that finished produced.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    pub console: String,
    pub console_bytes_dropped: usize,
    pub ops: u64,
    pub ms: u64,
    /// The program's completion value, if it produced one. NOT model-visible: console is.
    pub value: Option<serde_json::Value>,
}

/// A short, stable digest of a program's source, so an invariant violation can name the program
/// it belongs to without carrying the whole source around.
pub fn digest(source: &str) -> String {
    // FNV-1a, 64-bit: not cryptography, just a label.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in source.as_bytes() {
        h ^= *b as u64;
        h = h.wrapping_mul(0x1000_0000_01b3);
    }
    format!("{h:016x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_is_stable_and_distinguishes() {
        assert_eq!(digest("await bash('ls')"), digest("await bash('ls')"));
        assert_ne!(digest("await bash('ls')"), digest("await bash('pwd')"));
        assert_eq!(digest("").len(), 16);
    }
}
