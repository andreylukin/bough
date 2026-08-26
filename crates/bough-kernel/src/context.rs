//! Invariant: a `Context` is a cheap clone that never re-owns. Whatever clone registers an effect,
//! the effect belongs to the same fiber, and reads go through that fiber's COMMITTED view — the
//! immutable snapshot of resolved bindings captured at activation — so a plugin sees the same
//! providers for its whole life, teardown included (§0.3). The capability check happens at the
//! point of use, against the effective inject set, BEFORE the store is consulted: an undeclared
//! read is `UndeclaredService` even when the key happens to be bound.

use std::any::Any;
use std::collections::{BTreeMap, HashMap};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::config::{Entry, Inject, RealmLabel};
use crate::effect::{Accumulator, EffectCtx, EffectHandle};
use crate::error::{KernelError, PluginError};
use crate::event::{
    self, EmitEvent, ListenerOpts, Next, ParallelEvent, Registry, SerialEvent, WaterfallEvent,
};
use crate::fiber::{EntryId, FiberHandle, FiberUid};
use crate::kernel::Kernel;
use crate::scope::ScopeKey;
use crate::service::{Binding, ProviderUid, ServiceKey, ServiceSlot, Store};

/// The realm every key resolves in unless an `isolate:` map says otherwise.
pub fn default_realm() -> RealmLabel {
    RealmLabel::new("default")
}

// ---------------------------------------------------------------------------
// KernelCore — the state every context shares
// ---------------------------------------------------------------------------

/// The kernel's shared state: the binding store, the listener registry, and one effect
/// accumulator per fiber. Split out from [`Kernel`] so WP-2's substrate stands on its own and
/// WP-3's lifecycle drives it from the outside; `Kernel` owns an `Arc<KernelCore>`.
pub struct KernelCore {
    pub(crate) store: Store,
    pub(crate) events: Registry,
    /// Per-fiber effect accumulators. LIFO within a fiber (§0.3).
    fibers: Mutex<HashMap<FiberUid, Arc<Accumulator>>>,
    next_fiber: AtomicU64,
}

impl KernelCore {
    pub fn new() -> Arc<KernelCore> {
        Arc::new(KernelCore {
            store: Store::default(),
            events: Registry::default(),
            fibers: Mutex::new(HashMap::new()),
            next_fiber: AtomicU64::new(1),
        })
    }

    /// A fresh fiber identity. A rebuild takes a new one; a reload of the same row does not.
    pub fn new_fiber_uid(&self) -> FiberUid {
        FiberUid(self.next_fiber.fetch_add(1, Ordering::SeqCst))
    }

    fn accumulator(&self, fiber: FiberUid) -> Arc<Accumulator> {
        self.fibers
            .lock()
            .entry(fiber)
            .or_insert_with(|| Arc::new(Accumulator::default()))
            .clone()
    }

    /// Attach an effect to a fiber's accumulator directly. Used by `create_scope`, which owns an
    /// effect before any context is tagged with it.
    pub fn push_fiber_effect(&self, fiber: FiberUid, handle: EffectHandle) {
        self.accumulator(fiber).push(handle);
    }

    /// Step one of UNLOADING (§0.3): the fiber stops providing before any inverse of its runs.
    /// Returns the service NAMEs that were withdrawn, so the caller can notify dependents.
    pub fn withdraw_bindings_of(&self, fiber: FiberUid) -> Vec<&'static str> {
        self.store.withdraw_fiber(fiber)
    }

    /// The last step of UNLOADING: unwind this fiber's accumulator, LIFO.
    pub async fn unwind_fiber(&self, fiber: FiberUid) {
        let acc = self.fibers.lock().remove(&fiber);
        if let Some(acc) = acc {
            acc.unwind().await;
        }
    }

    /// The service NAMEs `fiber` provides right now (for `RowSnapshot::provides`).
    pub fn provided_by(&self, fiber: FiberUid) -> Vec<&'static str> {
        self.store.provided_by(fiber)
    }

    /// How many listeners are registered for an event NAME. Diagnostics and swap tests.
    pub fn listener_count(&self, event: &'static str) -> usize {
        self.events.count(event)
    }

    /// How many bindings are live. Diagnostics and swap tests.
    pub fn binding_count(&self) -> usize {
        self.store.len()
    }
}

// ---------------------------------------------------------------------------
// CommittedView
// ---------------------------------------------------------------------------

/// The immutable snapshot of resolved bindings captured when a fiber activates (§0.3). A plugin
/// reads through it for its whole life, teardown included, so a provider that goes away mid-life
/// cannot yank a value out from under a plugin that is tearing down.
#[derive(Default)]
pub struct CommittedView {
    bindings: BTreeMap<String, Binding>,
}

impl CommittedView {
    /// Resolve every name in `names` against the live store, in `realms`/`scope`, and freeze it.
    pub fn capture(
        core: &KernelCore,
        names: &[&str],
        realms: &BTreeMap<String, RealmLabel>,
        scope: Option<&ScopeKey>,
    ) -> CommittedView {
        let mut bindings = BTreeMap::new();
        for name in names {
            if let Some(b) = resolve_live(core, name, realms, scope) {
                bindings.insert((*name).to_string(), b);
            }
        }
        CommittedView { bindings }
    }

    /// The `ProviderUid` this view resolved `name` to, if any. A change here is a reload (§0.3).
    pub fn provider_of(&self, name: &str) -> Option<ProviderUid> {
        self.bindings.get(name).map(|b| b.uid)
    }

    /// Every resolved name, in NAME order.
    pub fn names(&self) -> Vec<&str> {
        self.bindings.keys().map(String::as_str).collect()
    }

    fn get(&self, name: &str) -> Option<Binding> {
        self.bindings.get(name).cloned()
    }
}

/// Resolve one name against the live store: the realm from `realms`, then the scope chain nearest
/// first, then the untagged global binding (views inherit DOWN, §0.3).
fn resolve_live(
    core: &KernelCore,
    name: &str,
    realms: &BTreeMap<String, RealmLabel>,
    scope: Option<&ScopeKey>,
) -> Option<Binding> {
    let realm = realms.get(name).cloned().unwrap_or_else(default_realm);
    // `StoreKey` holds a `&'static str`; the caller's `name` borrows from one, so look the static
    // one up through the store's own keys by comparing the string content.
    let mut chain: Vec<Option<ScopeKey>> = Vec::new();
    if let Some(sk) = scope {
        for a in sk.ancestors() {
            chain.push(Some(a.clone()));
        }
    }
    chain.push(None);
    for sc in chain {
        if let Some(b) = core.store.get_by_name(&realm, name, sc.as_ref()) {
            return Some(b);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Context
// ---------------------------------------------------------------------------

struct ScopeSlot {
    key: ScopeKey,
    effect: EffectHandle,
}

struct Inner {
    core: Arc<KernelCore>,
    /// The kernel handle, shared by every context derived from the root. It is a `Weak` in a
    /// shared cell because the kernel owns the root context: the cell is filled once, after the
    /// `Arc<Kernel>` exists, and every context derived before OR after that moment sees it.
    kernel: Arc<std::sync::OnceLock<std::sync::Weak<Kernel>>>,
    fiber: FiberUid,
    entry: EntryId,
    plugin: &'static str,
    /// service NAME → realm, from the row's `isolate:` map and from `Context::isolate`.
    realms: BTreeMap<String, RealmLabel>,
    /// The row's effective inject set (entry ∪ plugin-static, Decision D1).
    inject: Inject,
    /// Per-context interception metadata, shared by every clone of this context.
    intercepts: Arc<RwLock<HashMap<String, Arc<serde_yaml::Value>>>>,
    view: Option<Arc<CommittedView>>,
    scope: Option<Arc<ScopeSlot>>,
}

/// The handle a plugin is given. Carries the owning `FiberUid`, the realm map from `isolate:`, the
/// interception map, and the scope chain.
#[derive(Clone)]
pub struct Context {
    inner: Arc<Inner>,
}

impl Context {
    // ---- construction (the seam WP-3 drives) -------------------------------

    /// The root context: fiber 0, row `root`, no declarations. Every row context descends from it.
    pub fn root(core: Arc<KernelCore>) -> Context {
        Context {
            inner: Arc::new(Inner {
                core,
                kernel: Arc::new(std::sync::OnceLock::new()),
                fiber: FiberUid(0),
                entry: EntryId::new("root"),
                plugin: "kernel",
                realms: BTreeMap::new(),
                inject: Inject::none(),
                intercepts: Arc::new(RwLock::new(HashMap::new())),
                view: None,
                scope: None,
            }),
        }
    }

    fn derive(&self, f: impl FnOnce(&mut Inner)) -> Context {
        let mut next = Inner {
            core: self.inner.core.clone(),
            kernel: self.inner.kernel.clone(),
            fiber: self.inner.fiber,
            entry: self.inner.entry.clone(),
            plugin: self.inner.plugin,
            realms: self.inner.realms.clone(),
            inject: self.inner.inject.clone(),
            intercepts: self.inner.intercepts.clone(),
            view: self.inner.view.clone(),
            scope: self.inner.scope.clone(),
        };
        f(&mut next);
        Context {
            inner: Arc::new(next),
        }
    }

    /// Attach the kernel handle. Called once by the kernel when it builds its root context; the
    /// handle lands in a cell shared with every context already derived from this one.
    pub fn with_kernel(&self, kernel: Arc<Kernel>) -> Context {
        let _ = self.inner.kernel.set(Arc::downgrade(&kernel));
        self.clone()
    }

    /// The context of one row: its own fiber, row id, plugin name and effective inject set.
    pub fn for_row(
        &self,
        fiber: FiberUid,
        entry: EntryId,
        plugin: &'static str,
        inject: Inject,
    ) -> Context {
        self.derive(|i| {
            i.fiber = fiber;
            i.entry = entry;
            i.plugin = plugin;
            i.inject = inject;
            i.view = None;
            i.scope = None;
            i.intercepts = Arc::new(RwLock::new(HashMap::new()));
        })
    }

    /// The row's `isolate:` map: service NAME → realm.
    pub fn with_realms(&self, realms: BTreeMap<String, RealmLabel>) -> Context {
        self.derive(|i| i.realms = realms)
    }

    /// Hand the fiber its committed view. Done once, at activation (§0.3).
    pub fn with_view(&self, view: Arc<CommittedView>) -> Context {
        self.derive(|i| i.view = Some(view))
    }

    pub(crate) fn with_scope(&self, key: ScopeKey, effect: EffectHandle) -> Context {
        self.derive(|i| i.scope = Some(Arc::new(ScopeSlot { key, effect })))
    }

    // ---- identity ---------------------------------------------------------

    /// The fiber that owns every effect registered through this context.
    pub fn fiber(&self) -> FiberHandle {
        // WP-3: the lifecycle owns fibers; the context carries only the uid.
        self.kernel()
            .runtime()
            .handle(self.inner.fiber)
            .expect("the fiber that owns this context is live")
    }
    /// The uid of the fiber that owns every effect registered through this context.
    pub fn fiber_uid(&self) -> FiberUid {
        self.inner.fiber
    }
    /// The row id this context belongs to.
    pub fn entry_id(&self) -> &EntryId {
        &self.inner.entry
    }
    /// The catalog name of the plugin on this row.
    pub fn plugin_name(&self) -> &'static str {
        self.inner.plugin
    }
    /// The kernel this context belongs to.
    pub fn kernel(&self) -> Arc<Kernel> {
        self.inner
            .kernel
            .get()
            .and_then(std::sync::Weak::upgrade)
            .expect("context is not attached to a Kernel")
    }
    /// The shared state behind this context.
    pub fn core(&self) -> &Arc<KernelCore> {
        &self.inner.core
    }
    /// The scope this context is tagged with, if any.
    pub fn scope_key(&self) -> Option<&ScopeKey> {
        self.inner.scope.as_ref().map(|s| &s.key)
    }
    /// The row's effective inject set.
    pub fn inject(&self) -> &Inject {
        &self.inner.inject
    }
    /// This context's committed view, once activation has handed it one.
    pub fn view(&self) -> Option<&Arc<CommittedView>> {
        self.inner.view.as_ref()
    }
    /// The realm `name` resolves in for this context.
    pub fn realm_for(&self, name: &str) -> RealmLabel {
        self.inner
            .realms
            .get(name)
            .cloned()
            .unwrap_or_else(default_realm)
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
        let core = self.inner.core.clone();
        let key = (self.realm_for(K::NAME), K::NAME, self.scope_key().cloned());
        let uid = ProviderUid {
            fiber: self.inner.fiber,
            seq: core.store.next_seq(),
        };
        core.store.insert(
            key.clone(),
            Binding {
                uid,
                value: Arc::new(value),
            },
        );
        let uid_cell = Arc::new(Mutex::new(uid));
        let (c2, k2, cell) = (core.clone(), key.clone(), uid_cell.clone());
        let effect = self
            .effect(move |e| async move {
                e.defer_sync(move || {
                    let current = *cell.lock();
                    c2.store.remove_if(&k2, current);
                });
                Ok(())
            })
            .await
            .expect("a provision's registration effect cannot fail");
        Ok(ServiceSlot {
            core,
            key,
            uid: uid_cell,
            effect,
            _marker: std::marker::PhantomData,
        })
    }

    /// Read `K` from this fiber's committed view.
    ///
    /// `Err(UndeclaredService)` if `K::NAME` is in neither the fiber's effective inject set nor its
    /// own provisions. `Err(ServiceUnavailable)` if declared optional and absent.
    pub fn get<K: ServiceKey>(&self) -> Result<Arc<K::Value>, KernelError> {
        self.check_declared::<K>()?;
        match self.resolve(K::NAME).and_then(downcast::<K>) {
            Some(v) => Ok(v),
            None => Err(KernelError::ServiceUnavailable {
                plugin: self.inner.plugin,
                entry: self.inner.entry.clone(),
                key: K::NAME,
            }),
        }
    }

    /// As [`Context::get`], but an optional key that is absent is `Ok(None)`.
    pub fn try_get<K: ServiceKey>(&self) -> Result<Option<Arc<K::Value>>, KernelError> {
        self.check_declared::<K>()?;
        Ok(self.resolve(K::NAME).and_then(downcast::<K>))
    }

    /// The live store, bypassing the committed view. Only the kernel's own diagnostics and the
    /// launcher use this; a plugin calling it is a review failure.
    pub fn peek_live<K: ServiceKey>(&self) -> Option<Arc<K::Value>> {
        resolve_live(
            &self.inner.core,
            K::NAME,
            &self.inner.realms,
            self.scope_key(),
        )
        .and_then(downcast::<K>)
    }

    /// The capability check of §0.3, at the point of use and BEFORE the store is consulted.
    fn check_declared<K: ServiceKey>(&self) -> Result<(), KernelError> {
        if self.inner.inject.declares(K::NAME)
            || self
                .inner
                .core
                .store
                .fiber_provides(self.inner.fiber, K::NAME)
        {
            return Ok(());
        }
        Err(KernelError::UndeclaredService {
            plugin: self.inner.plugin,
            entry: self.inner.entry.clone(),
            key: K::NAME,
        })
    }

    /// Committed view first (a scoped context always resolves live, since its scope chain is not
    /// the one the view was captured against), then the live store.
    fn resolve(&self, name: &str) -> Option<Binding> {
        if self.inner.scope.is_none() {
            if let Some(view) = &self.inner.view {
                if let Some(b) = view.get(name) {
                    return Some(b);
                }
            }
        }
        resolve_live(&self.inner.core, name, &self.inner.realms, self.scope_key())
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
        let (handle, inner) = EffectHandle::new();
        let ectx = EffectCtx::new(self.clone(), inner);
        match body(ectx).await {
            Ok(()) => {
                self.register_effect(handle.clone());
                Ok(handle)
            }
            Err(e) => {
                // A failed body still unwinds whatever it had already deferred.
                handle.dispose().await;
                Err(e)
            }
        }
    }

    /// Spawn `body` and return immediately. Disposal halts it at its next
    /// [`EffectCtx::checkpoint`], then unwinds whatever it deferred, LIFO.
    pub fn effect_spawn<F, Fut>(&self, body: F) -> EffectHandle
    where
        F: FnOnce(EffectCtx) -> Fut + Send + 'static,
        Fut: Future<Output = Result<(), PluginError>> + Send + 'static,
    {
        let (handle, inner) = EffectHandle::new();
        let ectx = EffectCtx::new(self.clone(), inner);
        let entry = self.inner.entry.clone();
        let task = tokio::spawn(async move {
            if let Err(e) = body(ectx).await {
                tracing::warn!(%entry, error = %e, "spawned effect body failed");
            }
        });
        handle.attach_task(task);
        self.register_effect(handle.clone());
        handle
    }

    /// Attach a registration's lifetime to whatever owns this context: the scope it is tagged
    /// with, else the fiber's accumulator.
    fn register_effect(&self, handle: EffectHandle) {
        match &self.inner.scope {
            Some(scope) => scope.effect.defer_dispose(handle),
            None => self.inner.core.accumulator(self.inner.fiber).push(handle),
        }
    }

    // ---- nested mounts ----------------------------------------------------

    /// Mount `entry` as a child of this fiber. Children are effects of the parent, so unloading
    /// the parent cascades (§0.3).
    pub async fn mount(&self, entry: Entry) -> Result<FiberHandle, KernelError> {
        // WP-3: a nested mount is a fiber whose parent is this one, so the parent's teardown
        // cascades to it at its position in the accumulator (§0.3).
        let kernel = self.kernel();
        kernel.mount_child(self.inner.fiber, entry).await
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
        let core = self.inner.core.clone();
        let entry = self.inner.entry.clone();
        let scope = opts.scope.clone().or_else(|| self.scope_key().cloned());
        let id = event::register_emit::<E>(
            &core,
            entry,
            scope,
            opts.prepend,
            Arc::new(move |p| Box::pin(f(p))),
        );
        self.listener_effect(E::NAME, id).await
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
        let core = self.inner.core.clone();
        let entry = self.inner.entry.clone();
        let scope = opts.scope.clone().or_else(|| self.scope_key().cloned());
        let id = event::register_parallel::<E>(
            &core,
            entry,
            scope,
            opts.prepend,
            Arc::new(move |p| Box::pin(f(p))),
        );
        self.listener_effect(E::NAME, id).await
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
        let core = self.inner.core.clone();
        let entry = self.inner.entry.clone();
        let scope = opts.scope.clone().or_else(|| self.scope_key().cloned());
        let id = event::register_serial::<E>(
            &core,
            entry,
            scope,
            opts.prepend,
            Arc::new(move |p| Box::pin(f(p))),
        );
        self.listener_effect(E::NAME, id).await
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
        let core = self.inner.core.clone();
        let entry = self.inner.entry.clone();
        let scope = opts.scope.clone().or_else(|| self.scope_key().cloned());
        let id = event::register_waterfall::<E>(
            &core,
            entry,
            scope,
            opts.prepend,
            Arc::new(move |v, next| Box::pin(f(v, next))),
        );
        self.listener_effect(E::NAME, id).await
    }

    /// Every listener is an effect: its inverse removes it from the registry, so an unload leaves
    /// no listener behind (§0.2).
    async fn listener_effect(
        &self,
        event: &'static str,
        id: u64,
    ) -> Result<EffectHandle, PluginError> {
        let core = self.inner.core.clone();
        self.effect(move |e| async move {
            e.defer_sync(move || core.events.remove(event, id));
            Ok(())
        })
        .await
    }

    // ---- dispatch ---------------------------------------------------------

    /// Fire and forget; returns immediately.
    pub fn emit<E: EmitEvent>(&self, payload: E::Payload) {
        event::emit_ev::<E>(&self.inner.core, payload, None);
    }
    /// Start every listener concurrently; return when all have finished.
    pub async fn parallel<E: ParallelEvent>(&self, payload: E::Payload) {
        event::parallel_ev::<E>(&self.inner.core, payload, None).await
    }
    /// Run listeners in registration order; the first `Some` wins.
    pub async fn serial<E: SerialEvent>(&self, payload: E::Payload) -> Option<E::Output> {
        event::serial_ev::<E>(&self.inner.core, payload, None).await
    }
    /// Thread `value` through the chain; a listener that never calls `next` short-circuits it.
    pub async fn waterfall<E: WaterfallEvent>(&self, value: E::Value) -> E::Value {
        event::waterfall_ev::<E>(&self.inner.core, value, None).await
    }

    // ---- isolate / intercept (§0.3) ---------------------------------------

    /// A child context resolving `K` in `realm`. Entries sharing a realm label share the binding.
    pub fn isolate<K: ServiceKey>(&self, realm: RealmLabel) -> Context {
        self.derive(|i| {
            i.realms.insert(K::NAME.to_string(), realm);
        })
    }
    /// Per-context metadata a provider consults on use. Does NOT affect satisfaction and does NOT
    /// reload anyone; changeable at runtime.
    pub fn intercept<K: ServiceKey>(&self, metadata: serde_yaml::Value) -> Context {
        let child = self.derive(|i| {
            let map: HashMap<String, Arc<serde_yaml::Value>> = i.intercepts.read().clone();
            i.intercepts = Arc::new(RwLock::new(map));
        });
        child
            .inner
            .intercepts
            .write()
            .insert(K::NAME.to_string(), Arc::new(metadata));
        child
    }
    /// The metadata in force for `K` in this context, if any.
    pub fn interception<K: ServiceKey>(&self) -> Option<Arc<serde_yaml::Value>> {
        self.inner.intercepts.read().get(K::NAME).cloned()
    }
    /// Replace the metadata in force for `K` in this context.
    pub fn set_interception<K: ServiceKey>(&self, metadata: serde_yaml::Value) {
        self.inner
            .intercepts
            .write()
            .insert(K::NAME.to_string(), Arc::new(metadata));
    }
}

fn downcast<K: ServiceKey>(b: Binding) -> Option<Arc<K::Value>> {
    let any: Arc<dyn Any + Send + Sync> = b.value;
    any.downcast::<K::Value>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::tests::{row, Greeting};

    fn core() -> Arc<KernelCore> {
        KernelCore::new()
    }

    #[tokio::test]
    async fn isolate_gives_independent_bindings_per_realm() {
        let core = core();
        let a = row(&core, "a", "p", Inject::required(["greeting"]))
            .isolate::<Greeting>(RealmLabel::new("alpha"));
        let b = row(&core, "b", "p", Inject::required(["greeting"]))
            .isolate::<Greeting>(RealmLabel::new("beta"));

        a.provide::<Greeting>("from-alpha".into()).await.unwrap();
        b.provide::<Greeting>("from-beta".into()).await.unwrap();

        assert_eq!(*a.get::<Greeting>().unwrap(), "from-alpha");
        assert_eq!(*b.get::<Greeting>().unwrap(), "from-beta");

        // A row in the default realm sees neither.
        let c = row(&core, "c", "p", Inject::required(["greeting"]));
        assert!(c.get::<Greeting>().is_err());
    }

    #[tokio::test]
    async fn entries_sharing_a_realm_share_the_binding() {
        let core = core();
        let provider = row(&core, "provider", "p", Inject::none())
            .isolate::<Greeting>(RealmLabel::new("shared"));
        let consumer = row(&core, "consumer", "q", Inject::required(["greeting"]))
            .isolate::<Greeting>(RealmLabel::new("shared"));
        provider
            .provide::<Greeting>("one binding".into())
            .await
            .unwrap();
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "one binding");
    }

    #[tokio::test]
    async fn intercept_metadata_is_visible_to_the_consumer() {
        let core = core();
        let base = row(&core, "consumer", "q", Inject::required(["greeting"]));
        assert!(base.interception::<Greeting>().is_none());

        let tagged = base.intercept::<Greeting>(serde_yaml::Value::String("shout".into()));
        assert_eq!(
            tagged.interception::<Greeting>().as_deref(),
            Some(&serde_yaml::Value::String("shout".into()))
        );
        // The parent context is untouched: interception is per-context.
        assert!(base.interception::<Greeting>().is_none());
        // ...and every clone of the tagged context sees it.
        assert!(tagged.clone().interception::<Greeting>().is_some());
    }

    #[tokio::test]
    async fn intercept_change_does_not_reload() {
        let core = core();
        let provider = row(&core, "provider", "p", Inject::none());
        let slot = provider.provide::<Greeting>("hi".into()).await.unwrap();
        let before = slot.uid();

        let consumer = row(&core, "consumer", "q", Inject::required(["greeting"]))
            .intercept::<Greeting>(serde_yaml::Value::String("quiet".into()));
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "hi");

        consumer.set_interception::<Greeting>(serde_yaml::Value::String("loud".into()));
        assert_eq!(
            consumer.interception::<Greeting>().as_deref(),
            Some(&serde_yaml::Value::String("loud".into()))
        );
        // Satisfaction is untouched: same binding, same identity, nothing to reload.
        assert_eq!(slot.uid(), before);
        assert_eq!(consumer.view().map(|v| v.names().len()).unwrap_or(0), 0);
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "hi");
    }
}
