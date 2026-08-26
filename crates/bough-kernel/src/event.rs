//! Invariant: the dispatch mode of an event is part of its public contract and is checked by the
//! compiler — four traits, not one trait plus a runtime mode enum (§0.2, Decision D3). Every
//! listener invocation is contained: a panic or an `Err` is caught, `kernel/listener-failed` is
//! emitted, and the dispatch continues (§0.3, Decision D4).

use std::any::Any;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::RwLock;

use crate::context::KernelCore;
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
///
/// `Value: Clone` is what makes Decision D4's waterfall containment — *delegate unchanged* — even
/// expressible: a panicking listener has already consumed the value it was handed, so the runner
/// keeps a copy to continue the chain with. See the seam note in the Phase 0 report.
pub trait WaterfallEvent: Send + Sync + 'static {
    const NAME: &'static str;
    const MODE: DispatchMode = DispatchMode::Waterfall;
    type Value: Clone + Send + 'static;
}

// ---------------------------------------------------------------------------
// Listener storage
// ---------------------------------------------------------------------------

pub(crate) type EmitFn<E> =
    Arc<dyn Fn(<E as EmitEvent>::Payload) -> BoxFuture<'static, ()> + Send + Sync>;
pub(crate) type ParallelFn<E> =
    Arc<dyn Fn(<E as ParallelEvent>::Payload) -> BoxFuture<'static, ()> + Send + Sync>;
pub(crate) type SerialFn<E> = Arc<
    dyn Fn(<E as SerialEvent>::Payload) -> BoxFuture<'static, Option<<E as SerialEvent>::Output>>
        + Send
        + Sync,
>;
pub(crate) type WaterfallFn<E> = Arc<
    dyn Fn(
            <E as WaterfallEvent>::Value,
            Next<E>,
        ) -> BoxFuture<'static, <E as WaterfallEvent>::Value>
        + Send
        + Sync,
>;

/// One registered listener. The closure is stored type-erased and downcast at dispatch: the event
/// NAME plus the trait it was registered through fixes the type.
struct ListenerRec {
    id: u64,
    entry: EntryId,
    /// `None` = untagged: admitted by every dispatch. `Some(k)` = admitted only for a dispatch
    /// targeted at `k` or a descendant of `k` (admission extends UP, §0.3).
    scope: Option<ScopeKey>,
    f: Arc<dyn Any + Send + Sync>,
}

/// Every listener in the kernel, keyed by event NAME.
#[derive(Default)]
pub(crate) struct Registry {
    by_event: RwLock<HashMap<&'static str, Vec<ListenerRec>>>,
    seq: AtomicU64,
}

impl Registry {
    fn add(
        &self,
        event: &'static str,
        entry: EntryId,
        scope: Option<ScopeKey>,
        prepend: bool,
        f: Arc<dyn Any + Send + Sync>,
    ) -> u64 {
        let id = self.seq.fetch_add(1, Ordering::SeqCst);
        let rec = ListenerRec {
            id,
            entry,
            scope,
            f,
        };
        let mut map = self.by_event.write();
        let slot = map.entry(event).or_default();
        if prepend {
            slot.insert(0, rec);
        } else {
            slot.push(rec);
        }
        id
    }

    pub(crate) fn remove(&self, event: &'static str, id: u64) {
        if let Some(slot) = self.by_event.write().get_mut(event) {
            slot.retain(|l| l.id != id);
        }
    }

    /// The admitted listeners for one dispatch, in registration order, already downcast.
    fn admitted<T: Any + Send + Sync + Clone>(
        &self,
        event: &'static str,
        target: Option<&ScopeKey>,
    ) -> Vec<(EntryId, T)> {
        let map = self.by_event.read();
        let Some(slot) = map.get(event) else {
            return Vec::new();
        };
        slot.iter()
            .filter(|l| admits(l.scope.as_ref(), target))
            .filter_map(|l| {
                l.f.downcast_ref::<T>()
                    .map(|f| (l.entry.clone(), f.clone()))
            })
            .collect()
    }

    /// How many listeners are registered for `event`. Used by the scope tests and by WP-3's
    /// "leaves no listeners" assertions.
    pub(crate) fn count(&self, event: &'static str) -> usize {
        self.by_event.read().get(event).map_or(0, |v| v.len())
    }
}

/// Admission: untagged listeners always; a tagged listener only when the dispatch target is that
/// scope or a descendant of it. Never a sibling, never a descendant of the listener's own scope.
fn admits(listener: Option<&ScopeKey>, target: Option<&ScopeKey>) -> bool {
    match listener {
        None => true,
        Some(l) => target.is_some_and(|t| t.ancestors().any(|a| a == l)),
    }
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(crate) fn register_emit<E: EmitEvent>(
    core: &KernelCore,
    entry: EntryId,
    scope: Option<ScopeKey>,
    prepend: bool,
    f: EmitFn<E>,
) -> u64 {
    core.events.add(E::NAME, entry, scope, prepend, Arc::new(f))
}

pub(crate) fn register_parallel<E: ParallelEvent>(
    core: &KernelCore,
    entry: EntryId,
    scope: Option<ScopeKey>,
    prepend: bool,
    f: ParallelFn<E>,
) -> u64 {
    core.events.add(E::NAME, entry, scope, prepend, Arc::new(f))
}

pub(crate) fn register_serial<E: SerialEvent>(
    core: &KernelCore,
    entry: EntryId,
    scope: Option<ScopeKey>,
    prepend: bool,
    f: SerialFn<E>,
) -> u64 {
    core.events.add(E::NAME, entry, scope, prepend, Arc::new(f))
}

pub(crate) fn register_waterfall<E: WaterfallEvent>(
    core: &KernelCore,
    entry: EntryId,
    scope: Option<ScopeKey>,
    prepend: bool,
    f: WaterfallFn<E>,
) -> u64 {
    core.events.add(E::NAME, entry, scope, prepend, Arc::new(f))
}

// ---------------------------------------------------------------------------
// Containment (§0.3, Decision D4)
// ---------------------------------------------------------------------------

fn panic_detail(p: Box<dyn Any + Send>) -> String {
    if let Some(s) = p.downcast_ref::<&'static str>() {
        (*s).to_string()
    } else if let Some(s) = p.downcast_ref::<String>() {
        s.clone()
    } else {
        "panicked".to_string()
    }
}

/// Report a contained failure. Never recurses: a listener on `kernel/listener-failed` that itself
/// fails is logged and dropped, not re-broadcast.
fn report(core: &Arc<KernelCore>, event: &'static str, entry: EntryId, detail: String) {
    tracing::warn!(event, %entry, detail, "listener failed; contained");
    if event == ListenerFailed::NAME {
        return;
    }
    emit_ev::<ListenerFailed>(
        core,
        ListenerFailure {
            event,
            entry,
            detail,
        },
        None,
    );
}

// ---------------------------------------------------------------------------
// Dispatch
// ---------------------------------------------------------------------------

/// Fire and forget: returns immediately, listeners run in registration order on a dispatch task.
pub(crate) fn emit_ev<E: EmitEvent>(
    core: &Arc<KernelCore>,
    payload: E::Payload,
    target: Option<ScopeKey>,
) {
    let listeners = core.events.admitted::<EmitFn<E>>(E::NAME, target.as_ref());
    if listeners.is_empty() {
        return;
    }
    let core = core.clone();
    tokio::spawn(async move {
        for (entry, f) in listeners {
            let fut = f(payload.clone());
            if let Err(p) = AssertUnwindSafe(fut).catch_unwind().await {
                report(&core, E::NAME, entry, panic_detail(p));
            }
        }
    });
}

/// Awaited fan-out: every listener starts concurrently; returns when all have finished.
pub(crate) async fn parallel_ev<E: ParallelEvent>(
    core: &Arc<KernelCore>,
    payload: E::Payload,
    target: Option<ScopeKey>,
) {
    let listeners = core
        .events
        .admitted::<ParallelFn<E>>(E::NAME, target.as_ref());
    let runs = listeners.into_iter().map(|(entry, f)| {
        let core = core.clone();
        let payload = payload.clone();
        async move {
            let fut = f(payload);
            if let Err(p) = AssertUnwindSafe(fut).catch_unwind().await {
                report(&core, E::NAME, entry, panic_detail(p));
            }
        }
    });
    futures::future::join_all(runs).await;
}

/// Registration order; the first `Some` wins and the rest do not run. A contained failure counts
/// as `None` (Decision D4).
pub(crate) async fn serial_ev<E: SerialEvent>(
    core: &Arc<KernelCore>,
    payload: E::Payload,
    target: Option<ScopeKey>,
) -> Option<E::Output> {
    let listeners = core
        .events
        .admitted::<SerialFn<E>>(E::NAME, target.as_ref());
    for (entry, f) in listeners {
        let fut = f(payload.clone());
        match AssertUnwindSafe(fut).catch_unwind().await {
            Ok(Some(out)) => return Some(out),
            Ok(None) => {}
            Err(p) => report(core, E::NAME, entry, panic_detail(p)),
        }
    }
    None
}

/// The remainder of a waterfall chain. `run` consumes `self`, so "call `next` at most once" is a
/// type error rather than a runtime rule.
pub struct Next<E: WaterfallEvent> {
    core: Arc<KernelCore>,
    rest: Arc<Vec<(EntryId, WaterfallFn<E>)>>,
    idx: usize,
}

impl<E: WaterfallEvent> Next<E> {
    /// Run the remainder of the chain with `value`.
    pub async fn run(self, value: E::Value) -> E::Value {
        run_chain::<E>(self.core, self.rest, self.idx, value).await
    }
}

fn run_chain<E: WaterfallEvent>(
    core: Arc<KernelCore>,
    chain: Arc<Vec<(EntryId, WaterfallFn<E>)>>,
    idx: usize,
    value: E::Value,
) -> BoxFuture<'static, E::Value> {
    Box::pin(async move {
        let Some((entry, f)) = chain.get(idx).cloned() else {
            return value;
        };
        // Kept so a contained panic can still *delegate unchanged* (Decision D4).
        let unchanged = value.clone();
        let next = Next {
            core: core.clone(),
            rest: chain.clone(),
            idx: idx + 1,
        };
        let fut = f(value, next);
        match AssertUnwindSafe(fut).catch_unwind().await {
            Ok(v) => v,
            Err(p) => {
                report(&core, E::NAME, entry, panic_detail(p));
                run_chain::<E>(core, chain, idx + 1, unchanged).await
            }
        }
    })
}

/// Thread `value` through the admitted chain in registration order.
pub(crate) async fn waterfall_ev<E: WaterfallEvent>(
    core: &Arc<KernelCore>,
    value: E::Value,
    target: Option<ScopeKey>,
) -> E::Value {
    let listeners = core
        .events
        .admitted::<WaterfallFn<E>>(E::NAME, target.as_ref());
    run_chain::<E>(core.clone(), Arc::new(listeners), 0, value).await
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Context, KernelCore};
    use parking_lot::Mutex;
    use std::time::Duration;

    // ---- fixture events ---------------------------------------------------

    struct Ping;
    impl EmitEvent for Ping {
        const NAME: &'static str = "test/ping";
        type Payload = &'static str;
    }

    struct Fan;
    impl ParallelEvent for Fan {
        const NAME: &'static str = "test/fan";
        type Payload = ();
    }

    struct Ask;
    impl SerialEvent for Ask {
        const NAME: &'static str = "test/ask";
        type Payload = ();
        type Output = &'static str;
    }

    struct Flow;
    impl WaterfallEvent for Flow {
        const NAME: &'static str = "test/flow";
        type Value = String;
    }

    type Trace = Arc<Mutex<Vec<String>>>;
    fn trace() -> Trace {
        Arc::new(Mutex::new(Vec::new()))
    }
    fn root() -> Context {
        Context::root(KernelCore::new())
    }

    /// Emit is fire-and-forget, so a test waits for the dispatch task rather than sleeping blindly.
    async fn wait_until(f: impl Fn() -> bool) {
        for _ in 0..500 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("condition never became true");
    }

    #[tokio::test]
    async fn emit_runs_listeners_in_registration_order() {
        let ctx = root();
        let t = trace();
        for name in ["first", "second", "third"] {
            let t = t.clone();
            ctx.on::<Ping, _, _>(move |p| {
                let t = t.clone();
                async move { t.lock().push(format!("{name}:{p}")) }
            })
            .await
            .unwrap();
        }
        ctx.emit::<Ping>("hi");
        wait_until(|| t.lock().len() == 3).await;
        assert_eq!(&*t.lock(), &["first:hi", "second:hi", "third:hi"]);
    }

    #[tokio::test]
    async fn waterfall_threads_the_value() {
        let ctx = root();
        ctx.on_waterfall::<Flow, _, _>(|v, next| async move {
            let out = next.run(format!("{v}a")).await;
            format!("{out}A")
        })
        .await
        .unwrap();
        ctx.on_waterfall::<Flow, _, _>(|v, next| async move { next.run(format!("{v}b")).await })
            .await
            .unwrap();
        assert_eq!(ctx.waterfall::<Flow>("_".into()).await, "_abA");
    }

    #[tokio::test]
    async fn waterfall_short_circuits_when_next_is_skipped() {
        let ctx = root();
        let reached = Arc::new(Mutex::new(false));
        ctx.on_waterfall::<Flow, _, _>(|v, _next| async move { format!("{v}!") })
            .await
            .unwrap();
        let r = reached.clone();
        ctx.on_waterfall::<Flow, _, _>(move |v, next| {
            let r = r.clone();
            async move {
                *r.lock() = true;
                next.run(v).await
            }
        })
        .await
        .unwrap();
        assert_eq!(ctx.waterfall::<Flow>("x".into()).await, "x!");
        assert!(!*reached.lock(), "the rest of the chain ran anyway");
    }

    #[tokio::test]
    async fn waterfall_prepend_runs_first() {
        let ctx = root();
        ctx.on_waterfall::<Flow, _, _>(|v, next| async move { next.run(format!("{v}1")).await })
            .await
            .unwrap();
        ctx.on_waterfall_with::<Flow, _, _>(
            ListenerOpts {
                prepend: true,
                ..Default::default()
            },
            |v, next| async move { next.run(format!("{v}0")).await },
        )
        .await
        .unwrap();
        assert_eq!(ctx.waterfall::<Flow>(String::new()).await, "01");
    }

    #[tokio::test]
    async fn parallel_awaits_all_listeners() {
        let ctx = root();
        let t = trace();
        for (name, ms) in [("slow", 30u64), ("quick", 1)] {
            let t = t.clone();
            ctx.on_parallel::<Fan, _, _>(move |()| {
                let t = t.clone();
                async move {
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    t.lock().push(name.to_string());
                }
            })
            .await
            .unwrap();
        }
        ctx.parallel::<Fan>(()).await;
        // All finished by the time `parallel` returned, and they really did overlap.
        assert_eq!(&*t.lock(), &["quick", "slow"]);
    }

    #[tokio::test]
    async fn serial_returns_first_non_empty_in_order() {
        let ctx = root();
        ctx.on_serial::<Ask, _, _>(|()| async move { None })
            .await
            .unwrap();
        ctx.on_serial::<Ask, _, _>(|()| async move { Some("second") })
            .await
            .unwrap();
        assert_eq!(ctx.serial::<Ask>(()).await, Some("second"));
    }

    #[tokio::test]
    async fn serial_skips_later_listeners_after_a_hit() {
        let ctx = root();
        let ran = Arc::new(Mutex::new(false));
        ctx.on_serial::<Ask, _, _>(|()| async move { Some("first") })
            .await
            .unwrap();
        let r = ran.clone();
        ctx.on_serial::<Ask, _, _>(move |()| {
            let r = r.clone();
            async move {
                *r.lock() = true;
                Some("second")
            }
        })
        .await
        .unwrap();
        assert_eq!(ctx.serial::<Ask>(()).await, Some("first"));
        assert!(!*ran.lock(), "a later listener ran after a hit");
    }

    #[tokio::test]
    async fn panicking_listener_is_contained_in_every_mode() {
        let ctx = root();
        let t = trace();

        // emit
        ctx.on::<Ping, _, _>(|_| async move { panic!("boom-emit") })
            .await
            .unwrap();
        let te = t.clone();
        ctx.on::<Ping, _, _>(move |_| {
            let te = te.clone();
            async move { te.lock().push("emit-survivor".into()) }
        })
        .await
        .unwrap();
        ctx.emit::<Ping>("x");
        wait_until(|| t.lock().iter().any(|l| l == "emit-survivor")).await;

        // parallel
        ctx.on_parallel::<Fan, _, _>(|()| async move { panic!("boom-parallel") })
            .await
            .unwrap();
        let tp = t.clone();
        ctx.on_parallel::<Fan, _, _>(move |()| {
            let tp = tp.clone();
            async move { tp.lock().push("parallel-survivor".into()) }
        })
        .await
        .unwrap();
        ctx.parallel::<Fan>(()).await;
        assert!(t.lock().iter().any(|l| l == "parallel-survivor"));

        // serial: a contained failure counts as None
        ctx.on_serial::<Ask, _, _>(|()| async move { panic!("boom-serial") })
            .await
            .unwrap();
        ctx.on_serial::<Ask, _, _>(|()| async move { Some("serial-survivor") })
            .await
            .unwrap();
        assert_eq!(ctx.serial::<Ask>(()).await, Some("serial-survivor"));

        // waterfall: a contained failure delegates unchanged
        ctx.on_waterfall::<Flow, _, _>(|_v, _next| async move { panic!("boom-waterfall") })
            .await
            .unwrap();
        ctx.on_waterfall::<Flow, _, _>(|v, next| async move { next.run(format!("{v}tail")).await })
            .await
            .unwrap();
        assert_eq!(ctx.waterfall::<Flow>("head-".into()).await, "head-tail");
    }

    #[tokio::test]
    async fn contained_failure_emits_listener_failed() {
        let ctx = root();
        let seen: Arc<Mutex<Vec<ListenerFailure>>> = Arc::new(Mutex::new(Vec::new()));
        let s = seen.clone();
        ctx.on::<ListenerFailed, _, _>(move |f| {
            let s = s.clone();
            async move { s.lock().push(f) }
        })
        .await
        .unwrap();
        ctx.on_parallel::<Fan, _, _>(|()| async move { panic!("planted") })
            .await
            .unwrap();
        ctx.parallel::<Fan>(()).await;
        wait_until(|| !seen.lock().is_empty()).await;
        let f = seen.lock()[0].clone();
        assert_eq!(f.event, Fan::NAME);
        assert!(f.detail.contains("planted"), "detail was {:?}", f.detail);
    }
}
