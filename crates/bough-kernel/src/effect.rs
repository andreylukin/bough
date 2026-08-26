//! Invariant: a registration is an effect and every effect carries its own inverse. Inverses run
//! LIFO within an effect; effects run LIFO within a fiber. A disposer fires AT MOST ONCE however
//! many clones call it, halts an in-flight body at its next checkpoint, and still unwinds whatever
//! that body had already deferred (§0.3).

use std::future::Future;

use crate::context::Context;
use crate::error::PluginError;

/// The handle an effect body is given.
pub struct EffectCtx {
    _priv: (),
}

impl EffectCtx {
    /// The context the effect belongs to. Always the owning fiber's, whatever clone registered it.
    pub fn ctx(&self) -> &Context {
        todo!("WP-2")
    }
    /// Push an inverse. Inverses run LIFO within the effect.
    pub fn defer<F, Fut>(&self, inverse: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        todo!("WP-2")
    }
    /// Push a synchronous inverse.
    pub fn defer_sync(&self, inverse: impl FnOnce() + Send + 'static) {
        todo!("WP-2")
    }
    /// The halt boundary. Returns `Err(Halted)` once disposal has begun; a body that sees it must
    /// return promptly. A long-running `effect_spawn` body that never checkpoints cannot be halted.
    pub async fn checkpoint(&self) -> Result<(), Halted> {
        todo!("WP-2")
    }
    /// Non-awaiting form of [`EffectCtx::checkpoint`].
    pub fn is_halted(&self) -> bool {
        todo!("WP-2")
    }
}

/// Returned from a checkpoint once disposal has begun.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("effect halted by disposal")]
pub struct Halted;

/// A handle to one registered effect. Cheap to clone; every clone disposes the same effect.
#[derive(Clone)]
pub struct EffectHandle {
    _priv: (),
}

impl EffectHandle {
    /// Halt an in-flight body at its next checkpoint, then unwind its inverses LIFO.
    ///
    /// Fires at most once: an `AtomicBool` claims the run and later callers await the same
    /// completion rather than unwinding a second time.
    pub async fn dispose(&self) {
        todo!("WP-2")
    }
    /// Start disposal without awaiting it. For drop paths and sync call sites only.
    pub fn dispose_detached(&self) {
        todo!("WP-2")
    }
    /// Whether disposal has completed.
    pub fn is_disposed(&self) -> bool {
        todo!("WP-2")
    }
}

/// The per-effect and per-fiber accumulator. Private to the kernel; named here because the LIFO
/// ordering it enforces is the normative part of §0.3 that WP-2 must not reorganise away.
pub(crate) struct Accumulator;

/// Marker for a body that failed. The kernel logs it against the owning row.
pub(crate) type EffectResult = Result<(), PluginError>;
