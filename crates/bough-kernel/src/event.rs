//! Invariant: the dispatch mode of an event is part of its public contract and is checked by the
//! compiler — four traits, not one trait plus a runtime mode enum (§0.2, Decision D3). Every
//! listener invocation is contained: a panic or an `Err` is caught, `kernel/listener-failed` is
//! emitted, and the dispatch continues (§0.3, Decision D4).

use std::sync::Arc;

use crate::fiber::{EntryId, FiberState, FiberUid};
use crate::invariant::InvariantViolation;
use crate::kernel::UnresolvedRow;
use crate::scope::ScopeKey;

/// Which of the four dispatch shapes an event uses. Present for the `--dump-config` catalog
/// surface and for the §15 item 7 `cargo xtask` gate; dispatch itself is chosen by the trait.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
pub enum DispatchMode {
    Emit,
    Parallel,
    Serial,
    Waterfall,
}

/// Fire and forget. `emit` returns immediately; listeners run in registration order on the
/// kernel's dispatch task. No return value.
pub trait EmitEvent: Send + Sync + 'static {
    const NAME: &'static str;
    const MODE: DispatchMode = DispatchMode::Emit;
    type Payload: Clone + Send + Sync + 'static;
}

/// Awaited fan-out: all listeners start concurrently and `parallel` returns when all have
/// finished. No return value.
pub trait ParallelEvent: Send + Sync + 'static {
    const NAME: &'static str;
    const MODE: DispatchMode = DispatchMode::Parallel;
    type Payload: Clone + Send + Sync + 'static;
}

/// Awaited in registration order. The FIRST listener returning `Some` wins; the rest do not run.
pub trait SerialEvent: Send + Sync + 'static {
    const NAME: &'static str;
    const MODE: DispatchMode = DispatchMode::Serial;
    type Payload: Clone + Send + Sync + 'static;
    type Output: Send + 'static;
}

/// Around-middleware. A listener receives the value and `next` and MUST call `next` to delegate;
/// returning without calling it short-circuits the rest of the chain (§0.3).
pub trait WaterfallEvent: Send + Sync + 'static {
    const NAME: &'static str;
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Value: Send + 'static;
}

/// The remainder of a waterfall chain. `run` consumes `self`, so "call `next` at most once" is a
/// type error rather than a runtime rule.
pub struct Next<E: WaterfallEvent> {
    _marker: std::marker::PhantomData<fn() -> E>,
}

impl<E: WaterfallEvent> Next<E> {
    /// Run the remainder of the chain with `value`.
    pub async fn run(self, value: E::Value) -> E::Value {
        todo!("WP-2")
    }
}

/// Options for the `on_*_with` registration forms.
#[derive(Clone, Default, Debug)]
pub struct ListenerOpts {
    /// Run before listeners already registered, rather than after.
    pub prepend: bool,
    /// Admit this listener only for dispatches targeted at `scope` or a descendant of it.
    pub scope: Option<ScopeKey>,
}

// ---------------------------------------------------------------------------
// Kernel-owned events. This is the whole Phase 0 catalog; none of it carries domain vocabulary.
// ---------------------------------------------------------------------------

/// A candidate tree was rejected; the last good tree is still running (§0.3).
///
/// Spelled without a `kernel/` prefix because §0.3 names it verbatim.
pub struct ConfigUpdateFailed;
impl EmitEvent for ConfigUpdateFailed {
    const NAME: &'static str = "config-update-failed";
    type Payload = Arc<crate::error::ComposeError>;
}

/// A candidate tree was accepted and reconciled.
pub struct ConfigUpdated;
impl EmitEvent for ConfigUpdated {
    const NAME: &'static str = "config-updated";
    type Payload = crate::config::Fingerprint;
}

/// One fiber lifecycle transition.
#[derive(Clone, Debug)]
pub struct FiberStateChange {
    pub uid: FiberUid,
    pub id: EntryId,
    pub from: FiberState,
    pub to: FiberState,
    pub error: Option<Arc<crate::error::PluginError>>,
}

/// Emitted on every transition of the inertial lifecycle.
pub struct FiberStateChanged;
impl EmitEvent for FiberStateChanged {
    const NAME: &'static str = "kernel/fiber-state";
    type Payload = FiberStateChange;
}

/// After quiescence, the enabled rows that are not ACTIVE. Fatal at boot, loud at runtime
/// (Decision D12).
pub struct RowsUnresolved;
impl EmitEvent for RowsUnresolved {
    const NAME: &'static str = "kernel/rows-unresolved";
    type Payload = Arc<Vec<UnresolvedRow>>;
}

/// One contained listener panic or error.
#[derive(Clone, Debug)]
pub struct ListenerFailure {
    pub event: &'static str,
    pub entry: EntryId,
    pub detail: String,
}

/// Emitted by containment, in every dispatch mode.
pub struct ListenerFailed;
impl EmitEvent for ListenerFailed {
    const NAME: &'static str = "kernel/listener-failed";
    type Payload = ListenerFailure;
}

/// The invariant runner found a violation. A report, never an unload (§0.2).
pub struct InvariantViolated;
impl EmitEvent for InvariantViolated {
    const NAME: &'static str = "kernel/invariant-violated";
    type Payload = Arc<InvariantViolation>;
}
