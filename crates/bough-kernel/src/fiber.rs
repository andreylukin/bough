//! Invariant: the lifecycle is INERTIAL (§0.3). Each fiber has a driver task and a `target`; the
//! reconciler only ever writes `target`, and the driver runs a transition **to completion** before
//! re-reading it. A target that changes mid-transition is honoured after, never during — the
//! temptation to short-circuit "we are about to unload anyway" is exactly the bug this shape
//! exists to prevent.
//!
//! UNLOADING order is mandated: first remove every binding whose `ProviderUid.fiber` is this
//! fiber and notify dependents; then await every notified dependent's own teardown; only then
//! unwind this fiber's accumulator, LIFO. Group children are effects of the parent and are
//! disposed, LIFO, at their position in that accumulator.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::Notify;

use crate::config::{Inject, RealmLabel};
use crate::error::PluginError;
use crate::event::FiberStateChange;
use crate::service::ProviderUid;

bough_util::brand_id!(
    /// A row id, as written in a bundle or a patch.
    pub struct EntryId;
);

/// Identity of one fiber instance. A rebuild (an `id` or `plugin` change) yields a new one.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, serde::Serialize)]
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

/// The dependency targets a fiber resolved at activation: inject key → the identity of the
/// binding it resolved to (§0.3). A resolved `ProviderUid` that differs on recompute is what makes
/// a reload — a changed VALUE behind the same uid does not.
///
/// Distinct from [`crate::context::ResolvedTargets`], which is the frozen VALUES a plugin reads
/// through. The driver compares identities; the plugin reads values.
pub type ResolvedTargets = BTreeMap<String, Option<ProviderUid>>;

/// What a fiber runs. The kernel's plugin-backed implementation lives in `kernel.rs`; the tests in
/// this module drive a recording body, which is how the ORDER mandated by §0.3 is asserted.
#[async_trait::async_trait]
pub trait FiberBody: Send + Sync + 'static {
    /// The effective inject set: the entry's ∪ the plugin's static one (Decision D1).
    fn inject(&self) -> Inject;
    /// Capture the committed view and run `apply`. `Ok` ⇒ ACTIVE.
    async fn load(&self, view: Arc<ResolvedTargets>) -> Result<(), PluginError>;
    /// Remove every binding this fiber provides. Runs BEFORE dependents are notified and long
    /// before any other inverse of this fiber (§0.3).
    async fn withdraw(&self);
    /// Unwind this fiber's effect accumulator, LIFO.
    async fn unwind(&self);
    /// The service names this fiber currently provides; for `--dump-config` and the snapshot.
    fn provides(&self) -> Vec<&'static str> {
        Vec::new()
    }
    /// Told its fiber's identity once, at creation, before the first load.
    fn attach(&self, _uid: FiberUid) {}
    /// The row's context, once it has one. `None` for bodies that do not run a plugin.
    fn context(&self) -> Option<crate::context::Context> {
        None
    }
}

/// Resolves an inject key to the identity of whatever binding a fiber would see.
///
/// The kernel implements this over the live binding store; the tests implement it over a map.
pub trait Resolver: Send + Sync + 'static {
    fn resolve(&self, key: &str, realm: Option<&RealmLabel>) -> Option<ProviderUid>;
}

/// Where `kernel/fiber-state` goes. A seam so the driver never needs a `Context` of its own.
pub trait StateSink: Send + Sync + 'static {
    fn fiber_state(&self, change: FiberStateChange);
}

/// A sink that drops every transition. The kernel installs a real one.
pub struct NullStateSink;
impl StateSink for NullStateSink {
    fn fiber_state(&self, _change: FiberStateChange) {}
}

// ---------------------------------------------------------------------------
// The fiber itself
// ---------------------------------------------------------------------------

struct StateCell {
    state: FiberState,
    error: Option<Arc<PluginError>>,
    unmet: Vec<String>,
    view: Option<Arc<ResolvedTargets>>,
    /// The target generation this fiber has finished converging to.
    applied_gen: u64,
}

#[derive(Clone, Copy)]
struct TargetCell {
    /// Whether the row wants to be loaded at all (`disabled: false`).
    want: bool,
    /// Bumped by every reload request. Reaching a new generation means a full unload+load.
    gen: u64,
}

/// One fiber's state. Public because `FiberRuntime` hands them out; every mutator on it is
/// `pub(crate)`, so only the kernel writes a target.
pub struct Fiber {
    uid: FiberUid,
    id: EntryId,
    plugin: Option<&'static str>,
    realms: BTreeMap<String, RealmLabel>,
    body: Mutex<Arc<dyn FiberBody>>,
    state: Mutex<StateCell>,
    target: Mutex<TargetCell>,
    /// Group children, in mount order. Disposed LIFO with the rest of the accumulator.
    children: Mutex<Vec<FiberUid>>,
    /// Woken when `target` changes or a dependency may have.
    wake: Notify,
    /// Woken when a transition finishes.
    settled: Notify,
    /// Bumped once this fiber's teardown has completed; a provider waits on its dependents' bumps.
    unload_epoch: AtomicU64,
    /// True while the driver is inside a transition.
    busy: AtomicBool,
    stopped: AtomicBool,
}

impl Fiber {
    pub(crate) fn state(&self) -> FiberState {
        self.state.lock().state
    }
    pub(crate) fn unmet(&self) -> Vec<String> {
        self.state.lock().unmet.clone()
    }
    pub(crate) fn view(&self) -> Option<Arc<ResolvedTargets>> {
        self.state.lock().view.clone()
    }
    pub(crate) fn uid(&self) -> FiberUid {
        self.uid
    }
    pub(crate) fn id(&self) -> &EntryId {
        &self.id
    }
    pub(crate) fn plugin(&self) -> Option<&'static str> {
        self.plugin
    }
    pub(crate) fn realms(&self) -> BTreeMap<String, RealmLabel> {
        self.realms.clone()
    }
    pub(crate) fn provides(&self) -> Vec<&'static str> {
        self.body().provides()
    }
    pub(crate) fn body(&self) -> Arc<dyn FiberBody> {
        self.body.lock().clone()
    }
    /// Swap the body without touching the lifecycle. A reconfigure that the plugin absorbed still
    /// has to leave the fiber holding the config it was handed.
    pub(crate) fn set_body(&self, body: Arc<dyn FiberBody>) {
        *self.body.lock() = body;
    }
    pub(crate) fn children(&self) -> Vec<FiberUid> {
        self.children.lock().clone()
    }
    pub(crate) fn add_child(&self, child: FiberUid) {
        self.children.lock().push(child);
    }

    /// Write the target. This is the ONLY way the reconciler touches a fiber (§0.3).
    pub(crate) fn set_want(&self, want: bool) {
        {
            let mut t = self.target.lock();
            if t.want == want {
                return;
            }
            t.want = want;
        }
        self.wake.notify_waiters();
        self.wake.notify_one();
    }

    /// Request a full unload+load. Honoured after the in-flight transition, never during it.
    pub(crate) fn request_reload(&self) {
        {
            let mut t = self.target.lock();
            t.gen += 1;
            t.want = true;
        }
        self.wake.notify_waiters();
        self.wake.notify_one();
    }

    /// Nudge the driver to recompute its dependency targets (a provider appeared or vanished).
    pub(crate) fn poke(&self) {
        self.wake.notify_waiters();
        self.wake.notify_one();
    }

    fn settled_now(&self) -> bool {
        if self.busy.load(Ordering::SeqCst) {
            return false;
        }
        let t = *self.target.lock();
        let s = self.state.lock();
        match (t.want, s.state) {
            (true, FiberState::Active)
            | (true, FiberState::Pending)
            | (true, FiberState::Failed) => s.applied_gen == t.gen,
            (false, FiberState::Inactive) => true,
            _ => false,
        }
    }
}

/// The handle a plugin and the snapshot see. Cheap to clone.
#[derive(Clone)]
pub struct FiberHandle {
    pub(crate) inner: Arc<Fiber>,
}

impl FiberHandle {
    pub fn uid(&self) -> FiberUid {
        self.inner.uid
    }
    pub fn id(&self) -> &EntryId {
        &self.inner.id
    }
    /// `None` for a pure group row (Decision D18).
    pub fn plugin(&self) -> Option<&'static str> {
        self.inner.plugin
    }
    pub fn state(&self) -> FiberState {
        self.inner.state()
    }
    pub fn error(&self) -> Option<Arc<PluginError>> {
        self.inner.state.lock().error.clone()
    }
    /// Unmet required keys; empty unless PENDING.
    pub fn unmet(&self) -> Vec<String> {
        self.inner.state.lock().unmet.clone()
    }
    /// The committed view captured at the last activation.
    pub fn committed_view(&self) -> Option<Arc<ResolvedTargets>> {
        self.inner.view()
    }
    /// Await the end of any in-flight transition AND of the transition it is already targeting.
    pub async fn settled(&self) -> FiberState {
        loop {
            let waiter = self.inner.settled.notified();
            if self.inner.settled_now() || self.inner.stopped.load(Ordering::SeqCst) {
                return self.state();
            }
            let _ = tokio::time::timeout(Duration::from_millis(20), waiter).await;
        }
    }
}

// ---------------------------------------------------------------------------
// The runtime: the fiber registry and the driver loop
// ---------------------------------------------------------------------------

/// Owns every fiber and the one driver task each of them has.
pub struct FiberRuntime {
    fibers: Mutex<BTreeMap<FiberUid, Arc<Fiber>>>,
    order: Mutex<Vec<FiberUid>>,
    next_uid: AtomicU64,
    resolver: Arc<dyn Resolver>,
    sink: Arc<dyn StateSink>,
    self_ref: Mutex<Weak<FiberRuntime>>,
}

impl FiberRuntime {
    pub fn new(resolver: Arc<dyn Resolver>, sink: Arc<dyn StateSink>) -> Arc<FiberRuntime> {
        let rt = Arc::new(FiberRuntime {
            fibers: Mutex::new(BTreeMap::new()),
            order: Mutex::new(Vec::new()),
            next_uid: AtomicU64::new(1),
            resolver,
            sink,
            self_ref: Mutex::new(Weak::new()),
        });
        *rt.self_ref.lock() = Arc::downgrade(&rt);
        rt
    }

    pub fn get(&self, uid: FiberUid) -> Option<Arc<Fiber>> {
        self.fibers.lock().get(&uid).cloned()
    }

    pub fn handle(&self, uid: FiberUid) -> Option<FiberHandle> {
        self.get(uid).map(|inner| FiberHandle { inner })
    }

    /// Every live fiber, in creation order.
    pub fn all(&self) -> Vec<Arc<Fiber>> {
        let map = self.fibers.lock();
        self.order
            .lock()
            .iter()
            .filter_map(|u| map.get(u).cloned())
            .collect()
    }

    /// Create a fiber and start its driver. It begins UNLOADED and converges to `want`.
    pub fn create(
        self: &Arc<Self>,
        id: EntryId,
        plugin: Option<&'static str>,
        realms: BTreeMap<String, RealmLabel>,
        parent: Option<FiberUid>,
        body: Arc<dyn FiberBody>,
        want: bool,
    ) -> FiberHandle {
        let uid = FiberUid(self.next_uid.fetch_add(1, Ordering::SeqCst));
        let fiber = Arc::new(Fiber {
            uid,
            id,
            plugin,
            realms,
            body: Mutex::new(body.clone()),
            state: Mutex::new(StateCell {
                state: FiberState::Inactive,
                error: None,
                unmet: Vec::new(),
                view: None,
                applied_gen: 0,
            }),
            target: Mutex::new(TargetCell { want, gen: 1 }),
            children: Mutex::new(Vec::new()),
            wake: Notify::new(),
            settled: Notify::new(),
            unload_epoch: AtomicU64::new(0),
            busy: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        });
        body.attach(uid);
        self.fibers.lock().insert(uid, fiber.clone());
        self.order.lock().push(uid);
        if let Some(p) = parent {
            if let Some(parent_fiber) = self.get(p) {
                parent_fiber.add_child(uid);
            }
        }
        let rt = self.clone();
        let driven = fiber.clone();
        tokio::spawn(async move { drive(rt, driven).await });
        FiberHandle { inner: fiber }
    }

    /// The fibers whose committed view names a binding provided by `uid`.
    pub fn dependents(&self, uid: FiberUid) -> Vec<Arc<Fiber>> {
        self.all()
            .into_iter()
            .filter(|f| {
                f.uid != uid
                    && f.view()
                        .map(|v| v.values().any(|p| matches!(p, Some(p) if p.fiber == uid)))
                        .unwrap_or(false)
            })
            .collect()
    }

    /// Unload `uid` and remove it from the registry. Its driver stops.
    pub async fn dispose(self: &Arc<Self>, uid: FiberUid) {
        let Some(fiber) = self.get(uid) else { return };
        fiber.set_want(false);
        FiberHandle {
            inner: fiber.clone(),
        }
        .settled()
        .await;
        fiber.stopped.store(true, Ordering::SeqCst);
        fiber.wake.notify_waiters();
        fiber.settled.notify_waiters();
        self.fibers.lock().remove(&uid);
        self.order.lock().retain(|u| *u != uid);
    }

    /// True when no fiber is mid-transition and every fiber has reached its target.
    pub fn settled_now(&self) -> bool {
        self.all().iter().all(|f| f.settled_now())
    }

    /// Wake every fiber so it re-checks its dependency targets.
    pub fn poke_all(&self) {
        for f in self.all() {
            f.poke();
        }
    }

    pub(crate) fn resolve_view(&self, fiber: &Fiber) -> (Arc<ResolvedTargets>, Vec<String>) {
        let inject = fiber.body().inject();
        let mut view: ResolvedTargets = BTreeMap::new();
        let mut unmet = Vec::new();
        for key in inject.required.iter().chain(inject.optional.iter()) {
            let realm = fiber.realms.get(key.as_str());
            let found = self.resolver.resolve(key, realm);
            if found.is_none() && inject.required.contains(key) {
                unmet.push(key.clone());
            }
            view.insert(key.clone(), found);
        }
        (Arc::new(view), unmet)
    }
}

fn transition(rt: &FiberRuntime, fiber: &Fiber, to: FiberState, error: Option<Arc<PluginError>>) {
    let from = {
        let mut s = fiber.state.lock();
        let from = s.state;
        s.state = to;
        s.error = error.clone();
        from
    };
    rt.sink.fiber_state(FiberStateChange {
        uid: fiber.uid,
        id: fiber.id.clone(),
        from,
        to,
        error,
    });
}

/// The driver loop. It reads `target` ONCE at the top and then runs the whole transition; a target
/// written while a transition is in flight is honoured on the next pass. That is the inertia.
async fn drive(rt: Arc<FiberRuntime>, fiber: Arc<Fiber>) {
    loop {
        if fiber.stopped.load(Ordering::SeqCst) {
            return;
        }
        let waiter = fiber.wake.notified();
        let target = *fiber.target.lock();
        let (state, applied_gen) = {
            let s = fiber.state.lock();
            (s.state, s.applied_gen)
        };

        let work = if !target.want {
            !matches!(state, FiberState::Inactive)
        } else {
            match state {
                FiberState::Active | FiberState::Failed => applied_gen != target.gen,
                FiberState::Pending => {
                    applied_gen != target.gen || {
                        // A provider may have arrived while we were PENDING.
                        let (_, unmet) = rt.resolve_view(&fiber);
                        unmet.is_empty()
                    }
                }
                FiberState::Inactive => true,
                FiberState::Loading | FiberState::Unloading => false,
            }
        };

        if !work {
            fiber.settled.notify_waiters();
            let _ = tokio::time::timeout(Duration::from_millis(20), waiter).await;
            continue;
        }

        fiber.busy.store(true, Ordering::SeqCst);

        if !target.want {
            unload(&rt, &fiber, FiberState::Inactive).await;
        } else {
            // A reload is an unload run to completion, then a load. Never a short-circuit.
            if matches!(state, FiberState::Active | FiberState::Failed) {
                unload(&rt, &fiber, FiberState::Inactive).await;
            }
            let (view, unmet) = rt.resolve_view(&fiber);
            if !unmet.is_empty() {
                {
                    let mut s = fiber.state.lock();
                    s.unmet = unmet;
                    s.view = None;
                }
                transition(&rt, &fiber, FiberState::Pending, None);
            } else {
                transition(&rt, &fiber, FiberState::Loading, None);
                {
                    let mut s = fiber.state.lock();
                    s.unmet = Vec::new();
                    // The COMMITTED view: captured before `apply`, immutable for this life.
                    s.view = Some(view.clone());
                }
                let body = fiber.body();
                match body.load(view).await {
                    Ok(()) => transition(&rt, &fiber, FiberState::Active, None),
                    Err(e) => {
                        // A failed apply unwinds as if unloaded, then rests in FAILED.
                        let err = Arc::new(e);
                        transition(&rt, &fiber, FiberState::Unloading, Some(err.clone()));
                        teardown(&rt, &fiber).await;
                        transition(&rt, &fiber, FiberState::Failed, Some(err));
                    }
                }
            }
        }

        fiber.state.lock().applied_gen = target.gen;
        fiber.busy.store(false, Ordering::SeqCst);
        fiber.settled.notify_waiters();
        // Whatever this fiber provides or stopped providing, everyone else may need to recompute.
        rt.poke_all();
    }
}

async fn unload(rt: &Arc<FiberRuntime>, fiber: &Arc<Fiber>, end: FiberState) {
    transition(rt, fiber, FiberState::Unloading, None);
    teardown(rt, fiber).await;
    {
        let mut s = fiber.state.lock();
        s.view = None;
        s.unmet = Vec::new();
    }
    transition(rt, fiber, end, None);
}

/// The mandated UNLOADING order (§0.3), in one place so it cannot drift:
/// withdraw → notify dependents → await their teardown → cascade to children → unwind LIFO.
async fn teardown(rt: &Arc<FiberRuntime>, fiber: &Arc<Fiber>) {
    // 1. This fiber stops providing, before any other inverse of it runs.
    fiber.body().withdraw().await;

    // 2. Notify dependents, then 3. await each one's own teardown.
    let deps = rt.dependents(fiber.uid);
    let before: Vec<(Arc<Fiber>, u64)> = deps
        .into_iter()
        .map(|d| {
            let e = d.unload_epoch.load(Ordering::SeqCst);
            d.request_reload();
            (d, e)
        })
        .collect();
    for (dep, epoch) in before {
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            if dep.unload_epoch.load(Ordering::SeqCst) > epoch
                || dep.stopped.load(Ordering::SeqCst)
                || std::time::Instant::now() > deadline
            {
                break;
            }
            let w = dep.settled.notified();
            let _ = tokio::time::timeout(Duration::from_millis(5), w).await;
        }
    }

    // 4. Group children are effects of this fiber: disposed LIFO, at their position.
    let kids = fiber.children.lock().clone();
    for child in kids.into_iter().rev() {
        rt.dispose(child).await;
    }
    fiber.children.lock().clear();

    // 5. Only now, this fiber's own accumulator, LIFO.
    fiber.body().unwind().await;
    fiber.unload_epoch.fetch_add(1, Ordering::SeqCst);
    fiber.settled.notify_waiters();
}

/// Await quiescence over the whole registry, INCLUDING fibers a transition created.
pub async fn quiesce_runtime(rt: &Arc<FiberRuntime>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let mut stable = 0;
    loop {
        if rt.settled_now() {
            stable += 1;
            // Two consecutive clean passes, so a fiber created by the transition we just observed
            // is counted before we call it quiet.
            if stable >= 3 {
                return;
            }
        } else {
            stable = 0;
        }
        if std::time::Instant::now() > deadline {
            tracing::error!("quiesce timed out; the tree is still converging");
            return;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::BTreeSet;

    /// A shared, ordered trace of everything the test bodies did. Order IS the assertion here.
    #[derive(Clone, Default)]
    pub(crate) struct Trace(Arc<Mutex<Vec<String>>>);
    impl Trace {
        pub(crate) fn push(&self, s: impl Into<String>) {
            self.0.lock().push(s.into());
        }
        pub(crate) fn entries(&self) -> Vec<String> {
            self.0.lock().clone()
        }
        pub(crate) fn index_of(&self, s: &str) -> Option<usize> {
            self.entries().iter().position(|e| e == s)
        }
        pub(crate) fn count(&self, s: &str) -> usize {
            self.entries().iter().filter(|e| *e == s).count()
        }
    }

    /// A binding store good enough to resolve `ProviderUid`s: the kernel's own store is WP-2's.
    #[derive(Default)]
    pub(crate) struct TestStore {
        bindings: Mutex<BTreeMap<(String, Option<String>), ProviderUid>>,
        seq: AtomicU64,
    }

    impl TestStore {
        pub(crate) fn provide(&self, key: &str, realm: Option<&RealmLabel>, fiber: FiberUid) {
            let seq = self.seq.fetch_add(1, Ordering::SeqCst);
            self.bindings.lock().insert(
                (key.to_string(), realm.map(|r| r.as_str().to_string())),
                ProviderUid { fiber, seq },
            );
        }
        pub(crate) fn withdraw(&self, fiber: FiberUid) {
            self.bindings.lock().retain(|_, v| v.fiber != fiber);
        }
        pub(crate) fn len(&self) -> usize {
            self.bindings.lock().len()
        }
    }

    impl Resolver for TestStore {
        fn resolve(&self, key: &str, realm: Option<&RealmLabel>) -> Option<ProviderUid> {
            let b = self.bindings.lock();
            b.get(&(key.to_string(), realm.map(|r| r.as_str().to_string())))
                .copied()
                // A realm with no binding of its own falls back to the global one.
                .or_else(|| b.get(&(key.to_string(), None)).copied())
        }
    }

    /// A body that records what it did, provides a key, and can be made slow or failing.
    pub(crate) struct TestBody {
        pub name: String,
        pub trace: Trace,
        pub store: Arc<TestStore>,
        pub uid: Mutex<Option<FiberUid>>,
        pub provides: Option<&'static str>,
        pub inject: Inject,
        pub load_delay: Duration,
        pub unload_delay: Duration,
        pub fail: AtomicBool,
        pub realm: Option<RealmLabel>,
    }

    impl TestBody {
        pub(crate) fn new(name: &str, trace: &Trace, store: &Arc<TestStore>) -> Arc<TestBody> {
            Arc::new(TestBody {
                name: name.to_string(),
                trace: trace.clone(),
                store: store.clone(),
                uid: Mutex::new(None),
                provides: None,
                inject: Inject::none(),
                load_delay: Duration::ZERO,
                unload_delay: Duration::ZERO,
                fail: AtomicBool::new(false),
                realm: None,
            })
        }
    }

    #[async_trait::async_trait]
    impl FiberBody for TestBody {
        fn inject(&self) -> Inject {
            self.inject.clone()
        }
        fn provides(&self) -> Vec<&'static str> {
            self.provides.into_iter().collect()
        }
        fn attach(&self, uid: FiberUid) {
            *self.uid.lock() = Some(uid);
        }
        async fn load(&self, view: Arc<ResolvedTargets>) -> Result<(), PluginError> {
            self.trace.push(format!("{}:apply-start", self.name));
            if !self.load_delay.is_zero() {
                tokio::time::sleep(self.load_delay).await;
            }
            if self.fail.load(Ordering::SeqCst) {
                self.trace.push(format!("{}:apply-failed", self.name));
                return Err(PluginError::new(
                    EntryId::new(self.name.clone()),
                    anyhow::anyhow!("planted apply failure"),
                ));
            }
            if let Some(key) = self.provides {
                let uid = self.uid.lock().expect("uid set before load");
                self.store.provide(key, self.realm.as_ref(), uid);
                self.trace.push(format!("{}:provide", self.name));
            }
            let _ = view;
            self.trace.push(format!("{}:apply", self.name));
            Ok(())
        }
        async fn withdraw(&self) {
            if self.provides.is_some() {
                if let Some(uid) = *self.uid.lock() {
                    self.store.withdraw(uid);
                }
                self.trace.push(format!("{}:withdraw", self.name));
            }
        }
        async fn unwind(&self) {
            if !self.unload_delay.is_zero() {
                tokio::time::sleep(self.unload_delay).await;
            }
            self.trace.push(format!("{}:unwind", self.name));
        }
    }

    pub(crate) struct Harness {
        pub rt: Arc<FiberRuntime>,
        pub store: Arc<TestStore>,
        pub trace: Trace,
    }

    pub(crate) fn harness() -> Harness {
        let store = Arc::new(TestStore::default());
        let trace = Trace::default();
        let rt = FiberRuntime::new(store.clone(), Arc::new(NullStateSink));
        Harness { rt, store, trace }
    }

    impl Harness {
        pub(crate) fn spawn(&self, body: Arc<TestBody>, want: bool) -> FiberHandle {
            let id = EntryId::new(body.name.clone());
            self.rt
                .create(id, None, BTreeMap::new(), None, body.clone(), want)
        }
        pub(crate) async fn quiesce(&self) {
            quiesce_runtime(&self.rt).await;
        }
    }

    fn required(keys: &[&str]) -> Inject {
        Inject {
            required: keys.iter().map(|s| s.to_string()).collect::<BTreeSet<_>>(),
            optional: BTreeSet::new(),
        }
    }

    #[tokio::test]
    async fn pending_until_required_key_arrives() {
        let h = harness();
        let mut consumer = TestBody::new("consumer", &h.trace, &h.store);
        Arc::get_mut(&mut consumer).unwrap().inject = required(&["greeting"]);
        let c = h.spawn(consumer, true);
        h.quiesce().await;
        assert_eq!(c.state(), FiberState::Pending);
        assert_eq!(c.unmet(), vec!["greeting".to_string()]);

        let mut provider = TestBody::new("provider", &h.trace, &h.store);
        Arc::get_mut(&mut provider).unwrap().provides = Some("greeting");
        h.spawn(provider, true);
        h.quiesce().await;

        assert_eq!(c.state(), FiberState::Active);
        assert!(c.unmet().is_empty());
    }

    #[tokio::test]
    async fn activation_captures_the_committed_view() {
        let h = harness();
        let mut provider = TestBody::new("provider", &h.trace, &h.store);
        Arc::get_mut(&mut provider).unwrap().provides = Some("greeting");
        let p = h.spawn(provider, true);
        let mut consumer = TestBody::new("consumer", &h.trace, &h.store);
        Arc::get_mut(&mut consumer).unwrap().inject = required(&["greeting"]);
        let c = h.spawn(consumer, true);
        h.quiesce().await;

        let view = c
            .committed_view()
            .expect("an ACTIVE fiber has a committed view");
        let uid = view.get("greeting").unwrap().expect("resolved");
        assert_eq!(uid.fiber, p.uid());
    }

    #[tokio::test]
    async fn reload_runs_to_completion_before_new_target() {
        let h = harness();
        let mut body = TestBody::new("slow", &h.trace, &h.store);
        {
            let b = Arc::get_mut(&mut body).unwrap();
            b.load_delay = Duration::from_millis(40);
            b.unload_delay = Duration::from_millis(10);
        }
        let f = h.spawn(body, true);
        h.quiesce().await;
        h.trace.push("--");

        // Two reloads written back to back while the first is in flight.
        let fiber = h.rt.get(f.uid()).unwrap();
        fiber.request_reload();
        tokio::time::sleep(Duration::from_millis(5)).await;
        fiber.request_reload();
        h.quiesce().await;

        let t = h.trace.entries();
        let after: Vec<&str> = t
            .iter()
            .skip_while(|e| *e != "--")
            .skip(1)
            .map(|s| s.as_str())
            .collect();
        // Each transition ran to COMPLETION before the next target was read: the reload written
        // mid-flight produced a second whole cycle, never a truncated first one. Every
        // `apply-start` is followed by its `apply`.
        assert_eq!(
            after,
            vec![
                "slow:unwind",
                "slow:apply-start",
                "slow:apply",
                "slow:unwind",
                "slow:apply-start",
                "slow:apply"
            ],
            "trace: {t:?}"
        );
        assert_eq!(f.state(), FiberState::Active);
    }

    #[tokio::test]
    async fn unload_runs_to_completion_before_a_reload_target() {
        let h = harness();
        let mut body = TestBody::new("slow", &h.trace, &h.store);
        Arc::get_mut(&mut body).unwrap().unload_delay = Duration::from_millis(30);
        let f = h.spawn(body, true);
        h.quiesce().await;
        h.trace.push("--");

        let fiber = h.rt.get(f.uid()).unwrap();
        fiber.set_want(false);
        tokio::time::sleep(Duration::from_millis(5)).await;
        // A reload target arrives mid-unload: it must not truncate the unload.
        fiber.request_reload();
        h.quiesce().await;

        let t = h.trace.entries();
        let after: Vec<String> = t
            .iter()
            .skip_while(|e| *e != "--")
            .skip(1)
            .filter(|e| *e != "slow:apply-start")
            .cloned()
            .collect();
        assert_eq!(
            after,
            vec!["slow:unwind".to_string(), "slow:apply".to_string()]
        );
        assert_eq!(f.state(), FiberState::Active);
    }

    #[tokio::test]
    async fn provider_stops_providing_before_its_inverses_run() {
        let h = harness();
        let mut provider = TestBody::new("provider", &h.trace, &h.store);
        Arc::get_mut(&mut provider).unwrap().provides = Some("greeting");
        let p = h.spawn(provider, true);
        h.quiesce().await;
        assert_eq!(h.store.len(), 1);

        h.rt.get(p.uid()).unwrap().set_want(false);
        h.quiesce().await;

        let w = h.trace.index_of("provider:withdraw").unwrap();
        let u = h.trace.index_of("provider:unwind").unwrap();
        assert!(
            w < u,
            "withdraw must precede every other inverse: {:?}",
            h.trace.entries()
        );
        assert_eq!(h.store.len(), 0);
    }

    #[tokio::test]
    async fn dependents_tear_down_before_the_provider_unwinds() {
        let h = harness();
        let mut provider = TestBody::new("provider", &h.trace, &h.store);
        Arc::get_mut(&mut provider).unwrap().provides = Some("greeting");
        let p = h.spawn(provider, true);
        let mut consumer = TestBody::new("consumer", &h.trace, &h.store);
        Arc::get_mut(&mut consumer).unwrap().inject = required(&["greeting"]);
        let c = h.spawn(consumer, true);
        h.quiesce().await;
        assert_eq!(c.state(), FiberState::Active);
        h.trace.push("--");

        h.rt.get(p.uid()).unwrap().set_want(false);
        h.quiesce().await;

        let t = h.trace.entries();
        let base = t.iter().position(|e| e == "--").unwrap();
        let dep_unwind = t[base..]
            .iter()
            .position(|e| e == "consumer:unwind")
            .unwrap();
        let prov_unwind = t[base..]
            .iter()
            .position(|e| e == "provider:unwind")
            .unwrap();
        assert!(
            dep_unwind < prov_unwind,
            "the dependent must tear down before the provider unwinds: {t:?}"
        );
        assert_eq!(c.state(), FiberState::Pending);
        assert_eq!(c.unmet(), vec!["greeting".to_string()]);
    }

    #[tokio::test]
    async fn failed_apply_moves_to_failed_and_unwinds() {
        let h = harness();
        let body = TestBody::new("bad", &h.trace, &h.store);
        body.fail.store(true, Ordering::SeqCst);
        let f = h.spawn(body, true);
        h.quiesce().await;

        assert_eq!(f.state(), FiberState::Failed);
        assert!(f.error().is_some());
        assert_eq!(
            h.trace.count("bad:unwind"),
            1,
            "a failed apply unwinds as if unloaded"
        );
    }

    #[tokio::test]
    async fn group_children_are_effects_of_the_parent() {
        let h = harness();
        let parent_body = TestBody::new("parent", &h.trace, &h.store);
        let parent = h.spawn(parent_body, true);
        let child_body = TestBody::new("child", &h.trace, &h.store);
        let child = h.rt.create(
            EntryId::new("child"),
            None,
            BTreeMap::new(),
            Some(parent.uid()),
            child_body,
            true,
        );
        h.quiesce().await;

        assert_eq!(
            h.rt.get(parent.uid()).unwrap().children(),
            vec![child.uid()]
        );
        assert_eq!(child.state(), FiberState::Active);
    }

    #[tokio::test]
    async fn unloading_a_parent_cascades_to_group_children() {
        let h = harness();
        let parent_body = TestBody::new("parent", &h.trace, &h.store);
        let parent = h.spawn(parent_body, true);
        let a = TestBody::new("child-a", &h.trace, &h.store);
        let b = TestBody::new("child-b", &h.trace, &h.store);
        h.rt.create(
            EntryId::new("child-a"),
            None,
            BTreeMap::new(),
            Some(parent.uid()),
            a,
            true,
        );
        let child_b = h.rt.create(
            EntryId::new("child-b"),
            None,
            BTreeMap::new(),
            Some(parent.uid()),
            b,
            true,
        );
        h.quiesce().await;
        h.trace.push("--");

        h.rt.get(parent.uid()).unwrap().set_want(false);
        h.quiesce().await;

        let t = h.trace.entries();
        let base = t.iter().position(|e| e == "--").unwrap();
        let tail = &t[base..];
        let bi = tail
            .iter()
            .position(|e| e == "child-b:unwind")
            .expect("child b unwound");
        let ai = tail
            .iter()
            .position(|e| e == "child-a:unwind")
            .expect("child a unwound");
        let pi = tail.iter().position(|e| e == "parent:unwind").unwrap();
        assert!(bi < ai, "children unwind LIFO: {t:?}");
        assert!(
            ai < pi,
            "children unwind before the parent's own accumulator: {t:?}"
        );
        assert!(
            h.rt.get(child_b.uid()).is_none(),
            "a cascaded child is disposed"
        );
    }
}
