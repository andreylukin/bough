//! Invariant: a dependent targets a *binding identity* (`ProviderUid`), never a value (§0.3).
//! Overwriting a value in place is therefore invisible to dependents; withdraw-and-re-provide is
//! not. Every provision is an effect of the providing fiber and is withdrawn on unload, LIFO,
//! before any other inverse of that fiber runs.

use std::sync::Arc;

use crate::effect::EffectHandle;
use crate::fiber::FiberUid;

/// A capability slot.
///
/// `NAME` is the string that appears in `inject:` lists, in `isolate:` maps, in `--dump-config`
/// and in error messages. `Value` is `Sized` (Decision D5): a trait-object service is exposed as a
/// concrete handle newtype owned by the Service Definition, e.g.
/// `pub struct LedgerHandle(Arc<dyn Ledger>);`.
pub trait ServiceKey: Send + Sync + 'static {
    type Value: Send + Sync + 'static;
    const NAME: &'static str;
}

/// Identity of one binding. `seq` is bumped by `provide`/`republish` and left alone by `set`, so
/// the same fiber re-providing still reads as a change to its dependents (Decision D6).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize)]
pub struct ProviderUid {
    pub fiber: FiberUid,
    pub seq: u64,
}

/// Returned by [`crate::Context::provide`].
///
/// Dropping the slot does **not** withdraw the binding: the owning fiber's effect accumulator
/// does, LIFO, on unload.
pub struct ServiceSlot<K: ServiceKey> {
    _marker: std::marker::PhantomData<fn() -> K>,
}

impl<K: ServiceKey> ServiceSlot<K> {
    /// This binding's identity.
    pub fn uid(&self) -> ProviderUid {
        todo!("WP-2")
    }
    /// The effect that owns the provision; disposing it withdraws.
    pub fn effect(&self) -> &EffectHandle {
        todo!("WP-2")
    }
    /// Overwrite the value in place. Same `ProviderUid`; dependents are NOT notified (§0.3).
    pub fn set(&self, value: K::Value) {
        todo!("WP-2")
    }
    /// Withdraw and re-provide: a new `seq`, so dependents recompute and reload (§0.3).
    pub async fn republish(&self, value: K::Value) {
        todo!("WP-2")
    }
    /// Withdraw now. Idempotent.
    pub async fn withdraw(&self) {
        todo!("WP-2")
    }
}

/// One live binding in the store, as the kernel's own diagnostics see it.
#[derive(Clone)]
pub struct Binding {
    pub uid: ProviderUid,
    pub value: Arc<dyn std::any::Any + Send + Sync>,
}
