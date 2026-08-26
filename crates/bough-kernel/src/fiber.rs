//! Invariant: the lifecycle is INERTIAL (§0.3). Each fiber has a driver task and a `target`; the
//! reconciler only ever writes `target`, and the driver runs a transition **to completion** before
//! re-reading it. A target that changes mid-transition is honoured after, never during — the
//! temptation to short-circuit "we are about to unload anyway" is exactly the bug this shape
//! exists to prevent.
//!
//! UNLOADING order is mandated: first remove every binding whose `ProviderUid.fiber` is this
//! fiber and notify dependents; then await every notified dependent's own teardown; only then
//! unwind this fiber's accumulator, LIFO.

use std::sync::Arc;

use crate::error::PluginError;

bough_util::brand_id!(
    /// A row id, as written in a bundle or a patch.
    pub struct EntryId;
);

/// Identity of one fiber instance. A rebuild (an `id` or `plugin` change) yields a new one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize)]
pub struct FiberUid(pub u64);

/// `PENDING → LOADING → ACTIVE → UNLOADING → INACTIVE | FAILED`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
pub enum FiberState {
    /// A required key resolves to `None`.
    Pending,
    /// The committed view is captured and `apply` is running.
    Loading,
    /// `apply` returned `Ok`.
    Active,
    /// Bindings withdrawn, dependents notified, accumulator unwinding.
    Unloading,
    /// Unloaded cleanly.
    Inactive,
    /// `apply` returned `Err`; the fiber's effects were unwound as if unloaded.
    Failed,
}

/// A handle to one fiber. Cheap to clone.
#[derive(Clone)]
pub struct FiberHandle {
    _priv: (),
}

impl FiberHandle {
    pub fn uid(&self) -> FiberUid {
        todo!("WP-3")
    }
    pub fn id(&self) -> &EntryId {
        todo!("WP-3")
    }
    /// `None` for a pure group row (Decision D18).
    pub fn plugin(&self) -> Option<&'static str> {
        todo!("WP-3")
    }
    pub fn state(&self) -> FiberState {
        todo!("WP-3")
    }
    pub fn error(&self) -> Option<Arc<PluginError>> {
        todo!("WP-3")
    }
    /// Unmet required keys; empty unless PENDING.
    pub fn unmet(&self) -> Vec<String> {
        todo!("WP-3")
    }
    /// Await the end of any in-flight transition AND of the transition it is already targeting.
    pub async fn settled(&self) -> FiberState {
        todo!("WP-3")
    }
}
