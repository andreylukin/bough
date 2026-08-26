//! Invariant: a `Context` is a cheap clone that never re-owns. Whatever clone registers an effect,
//! the effect belongs to the same fiber, and reads go through that fiber's COMMITTED view — the
//! immutable snapshot of resolved bindings captured at activation — so a plugin sees the same
//! providers for its whole life, teardown included (§0.3). The capability check happens at the
//! point of use, against the effective inject set, BEFORE the store is consulted: an undeclared
//! read is `UndeclaredService` even when the key happens to be bound.

use std::future::Future;
use std::sync::Arc;

use crate::config::{Entry, RealmLabel};
use crate::effect::{EffectCtx, EffectHandle};
use crate::error::{KernelError, PluginError};
use crate::event::{EmitEvent, ListenerOpts, Next, ParallelEvent, SerialEvent, WaterfallEvent};
use crate::fiber::{EntryId, FiberHandle};
use crate::kernel::Kernel;
use crate::service::{ServiceKey, ServiceSlot};

/// The handle a plugin is given. Carries the owning `FiberUid`, the realm map from `isolate:`, the
/// interception map, and the scope chain.
#[derive(Clone)]
pub struct Context {
    _priv: Arc<()>,
}

impl Context {
    // ---- identity ---------------------------------------------------------

    /// The fiber that owns every effect registered through this context.
    pub fn fiber(&self) -> FiberHandle {
        todo!("WP-2")
    }
    /// The row id this context belongs to.
    pub fn entry_id(&self) -> &EntryId {
        todo!("WP-2")
    }
    /// The catalog name of the plugin on this row.
    pub fn plugin_name(&self) -> &'static str {
        todo!("WP-2")
    }
    /// The kernel this context belongs to.
    pub fn kernel(&self) -> &Kernel {
        todo!("WP-2")
    }

    // ---- services ---------------------------------------------------------

    /// Provide `K` in this context's realm for `K::NAME`.
    ///
    /// Registered as an effect of the owning fiber; withdrawn on unload BEFORE any other inverse
    /// of that fiber runs (§0.3).
    pub async fn provide<K: ServiceKey>(
        &self,
        value: K::Value,
    ) -> Result<ServiceSlot<K>, KernelError> {
        todo!("WP-2")
    }

    /// Read `K` from this fiber's committed view.
    ///
    /// `Err(UndeclaredService)` if `K::NAME` is in neither the fiber's effective inject set nor its
    /// own provisions. `Err(ServiceUnavailable)` if declared optional and absent.
    pub fn get<K: ServiceKey>(&self) -> Result<Arc<K::Value>, KernelError> {
        todo!("WP-2")
    }

    /// As [`Context::get`], but an optional key that is absent is `Ok(None)`.
    pub fn try_get<K: ServiceKey>(&self) -> Result<Option<Arc<K::Value>>, KernelError> {
        todo!("WP-2")
    }

    /// The live store, bypassing the committed view. Only the kernel's own diagnostics and the
    /// launcher use this; a plugin calling it is a review failure.
    pub fn peek_live<K: ServiceKey>(&self) -> Option<Arc<K::Value>> {
        todo!("WP-2")
    }

    // ---- effects ----------------------------------------------------------

    /// Run `body` to completion inline, then return.
    ///
    /// Inverses deferred inside `body` are prepended to the fiber's accumulator (LIFO recovery,
    /// §0.3). The 95% case: registering a service, a listener, a pane, a child entry.
    pub async fn effect<F, Fut>(&self, body: F) -> Result<EffectHandle, PluginError>
    where
        F: FnOnce(EffectCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), PluginError>> + Send + 'static,
    {
        todo!("WP-2")
    }

    /// Spawn `body` and return immediately. Disposal halts it at its next
    /// [`EffectCtx::checkpoint`], then unwinds whatever it deferred, LIFO.
    pub fn effect_spawn<F, Fut>(&self, body: F) -> EffectHandle
    where
        F: FnOnce(EffectCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), PluginError>> + Send + 'static,
    {
        todo!("WP-2")
    }

    // ---- nested mounts ----------------------------------------------------

    /// Mount `entry` as a child of this fiber. Children are effects of the parent, so unloading
    /// the parent cascades (§0.3).
    pub async fn mount(&self, entry: Entry) -> Result<FiberHandle, KernelError> {
        todo!("WP-3")
    }

    // ---- event registration -----------------------------------------------

    pub async fn on<E: EmitEvent, F, Fut>(&self, f: F) -> Result<EffectHandle, PluginError>
    where
        F: Fn(E::Payload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_with::<E, F, Fut>(ListenerOpts::default(), f).await
    }

    pub async fn on_parallel<E: ParallelEvent, F, Fut>(
        &self,
        f: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: Fn(E::Payload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.on_parallel_with::<E, F, Fut>(ListenerOpts::default(), f)
            .await
    }

    pub async fn on_serial<E: SerialEvent, F, Fut>(&self, f: F) -> Result<EffectHandle, PluginError>
    where
        F: Fn(E::Payload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<E::Output>> + Send + 'static,
    {
        self.on_serial_with::<E, F, Fut>(ListenerOpts::default(), f)
            .await
    }

    pub async fn on_waterfall<E: WaterfallEvent, F, Fut>(
        &self,
        f: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: Fn(E::Value, Next<E>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = E::Value> + Send + 'static,
    {
        self.on_waterfall_with::<E, F, Fut>(ListenerOpts::default(), f)
            .await
    }

    pub async fn on_with<E: EmitEvent, F, Fut>(
        &self,
        opts: ListenerOpts,
        f: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: Fn(E::Payload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        todo!("WP-2")
    }

    pub async fn on_parallel_with<E: ParallelEvent, F, Fut>(
        &self,
        opts: ListenerOpts,
        f: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: Fn(E::Payload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        todo!("WP-2")
    }

    pub async fn on_serial_with<E: SerialEvent, F, Fut>(
        &self,
        opts: ListenerOpts,
        f: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: Fn(E::Payload) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Option<E::Output>> + Send + 'static,
    {
        todo!("WP-2")
    }

    pub async fn on_waterfall_with<E: WaterfallEvent, F, Fut>(
        &self,
        opts: ListenerOpts,
        f: F,
    ) -> Result<EffectHandle, PluginError>
    where
        F: Fn(E::Value, Next<E>) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = E::Value> + Send + 'static,
    {
        todo!("WP-2")
    }

    // ---- dispatch ---------------------------------------------------------

    /// Fire and forget; returns immediately.
    pub fn emit<E: EmitEvent>(&self, payload: E::Payload) {
        todo!("WP-2")
    }
    /// Start every listener concurrently; return when all have finished.
    pub async fn parallel<E: ParallelEvent>(&self, payload: E::Payload) {
        todo!("WP-2")
    }
    /// Run listeners in registration order; the first `Some` wins.
    pub async fn serial<E: SerialEvent>(&self, payload: E::Payload) -> Option<E::Output> {
        todo!("WP-2")
    }
    /// Thread `value` through the chain; a listener that never calls `next` short-circuits it.
    pub async fn waterfall<E: WaterfallEvent>(&self, value: E::Value) -> E::Value {
        todo!("WP-2")
    }

    // ---- isolate / intercept (§0.3) ---------------------------------------

    /// A child context resolving `K` in `realm`. Entries sharing a realm label share the binding.
    pub fn isolate<K: ServiceKey>(&self, realm: RealmLabel) -> Context {
        todo!("WP-2")
    }
    /// Per-context metadata a provider consults on use. Does NOT affect satisfaction and does NOT
    /// reload anyone; changeable at runtime.
    pub fn intercept<K: ServiceKey>(&self, metadata: serde_yaml::Value) -> Context {
        todo!("WP-2")
    }
    /// The metadata in force for `K` in this context, if any.
    pub fn interception<K: ServiceKey>(&self) -> Option<Arc<serde_yaml::Value>> {
        todo!("WP-2")
    }
    /// Replace the metadata in force for `K` in this context.
    pub fn set_interception<K: ServiceKey>(&self, metadata: serde_yaml::Value) {
        todo!("WP-2")
    }
}
