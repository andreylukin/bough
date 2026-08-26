//! Invariant: a registration is an effect and every effect carries its own inverse. Inverses run
//! LIFO within an effect; effects run LIFO within a fiber. A disposer fires AT MOST ONCE however
//! many clones call it, halts an in-flight body at its next checkpoint, and still unwinds whatever
//! that body had already deferred (§0.3).

use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures::future::BoxFuture;
use parking_lot::Mutex;
use tokio::sync::Notify;
use tokio::task::JoinHandle;

use crate::context::Context;

/// One inverse pushed by an effect body.
enum Inverse {
    Async(Box<dyn FnOnce() -> BoxFuture<'static, ()> + Send>),
    Sync(Box<dyn FnOnce() + Send>),
}

impl Inverse {
    async fn run(self) {
        match self {
            Inverse::Async(f) => f().await,
            Inverse::Sync(f) => f(),
        }
    }
}

/// The shared state behind every clone of an [`EffectHandle`].
pub(crate) struct EffectInner {
    /// Pushed in registration order; unwound from the end (LIFO).
    inverses: Mutex<Vec<Inverse>>,
    /// Set the moment disposal begins, so an in-flight body sees it at its next checkpoint.
    halted: AtomicBool,
    /// Claims the single disposal run.
    claimed: AtomicBool,
    /// Set once the unwind has finished; waiters key off it.
    done: AtomicBool,
    finished: Notify,
    /// The spawned body, for `effect_spawn`. Awaited before the unwind so a halted body has
    /// finished deferring by the time its inverses run.
    task: Mutex<Option<JoinHandle<()>>>,
}

impl EffectInner {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inverses: Mutex::new(Vec::new()),
            halted: AtomicBool::new(false),
            claimed: AtomicBool::new(false),
            done: AtomicBool::new(false),
            finished: Notify::new(),
            task: Mutex::new(None),
        })
    }
}

/// The handle an effect body is given.
pub struct EffectCtx {
    ctx: Context,
    inner: Arc<EffectInner>,
}

impl EffectCtx {
    pub(crate) fn new(ctx: Context, inner: Arc<EffectInner>) -> Self {
        Self { ctx, inner }
    }

    /// The context the effect belongs to. Always the owning fiber's, whatever clone registered it.
    pub fn ctx(&self) -> &Context {
        &self.ctx
    }
    /// Push an inverse. Inverses run LIFO within the effect.
    pub fn defer<F, Fut>(&self, inverse: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.inner
            .inverses
            .lock()
            .push(Inverse::Async(Box::new(move || Box::pin(inverse()))));
    }
    /// Push a synchronous inverse.
    pub fn defer_sync(&self, inverse: impl FnOnce() + Send + 'static) {
        self.inner
            .inverses
            .lock()
            .push(Inverse::Sync(Box::new(inverse)));
    }
    /// The halt boundary. Returns `Err(Halted)` once disposal has begun; a body that sees it must
    /// return promptly. A long-running `effect_spawn` body that never checkpoints cannot be halted.
    pub async fn checkpoint(&self) -> Result<(), Halted> {
        if self.is_halted() {
            return Err(Halted);
        }
        tokio::task::yield_now().await;
        if self.is_halted() {
            return Err(Halted);
        }
        Ok(())
    }
    /// Non-awaiting form of [`EffectCtx::checkpoint`].
    pub fn is_halted(&self) -> bool {
        self.inner.halted.load(Ordering::SeqCst)
    }
}

/// Returned from a checkpoint once disposal has begun.
#[derive(Debug, Clone, Copy, thiserror::Error)]
#[error("effect halted by disposal")]
pub struct Halted;

/// A handle to one registered effect. Cheap to clone; every clone disposes the same effect.
#[derive(Clone)]
pub struct EffectHandle {
    inner: Arc<EffectInner>,
}

impl EffectHandle {
    pub(crate) fn new() -> (EffectHandle, Arc<EffectInner>) {
        let inner = EffectInner::new();
        (
            EffectHandle {
                inner: inner.clone(),
            },
            inner,
        )
    }

    /// Push "dispose `child`" as an inverse of this effect. How a scope, and a fiber's own
    /// accumulator, take ownership of a registration's lifetime.
    pub(crate) fn defer_dispose(&self, child: EffectHandle) {
        self.inner
            .inverses
            .lock()
            .push(Inverse::Async(Box::new(move || {
                Box::pin(async move { child.dispose().await })
            })));
    }

    pub(crate) fn attach_task(&self, task: JoinHandle<()>) {
        *self.inner.task.lock() = Some(task);
    }

    /// Halt an in-flight body at its next checkpoint, then unwind its inverses LIFO.
    ///
    /// Fires at most once: an `AtomicBool` claims the run and later callers await the same
    /// completion rather than unwinding a second time.
    pub async fn dispose(&self) {
        if self.inner.claimed.swap(true, Ordering::SeqCst) {
            // Someone else owns the run; await the same completion.
            loop {
                let waiting = self.inner.finished.notified();
                if self.inner.done.load(Ordering::SeqCst) {
                    return;
                }
                waiting.await;
            }
        }
        self.inner.halted.store(true, Ordering::SeqCst);
        let task = self.inner.task.lock().take();
        if let Some(task) = task {
            let _ = task.await;
        }
        let mut inverses: Vec<Inverse> = std::mem::take(&mut *self.inner.inverses.lock());
        while let Some(inv) = inverses.pop() {
            inv.run().await;
        }
        self.inner.done.store(true, Ordering::SeqCst);
        self.inner.finished.notify_waiters();
    }

    /// Start disposal without awaiting it. For drop paths and sync call sites only.
    pub fn dispose_detached(&self) {
        let me = self.clone();
        tokio::spawn(async move { me.dispose().await });
    }

    /// Whether disposal has completed.
    pub fn is_disposed(&self) -> bool {
        self.inner.done.load(Ordering::SeqCst)
    }

    /// Whether disposal has begun (the halt flag is set), completed or not.
    pub fn is_halting(&self) -> bool {
        self.inner.halted.load(Ordering::SeqCst)
    }
}

/// The per-fiber accumulator of effects. LIFO: the last effect registered is the first unwound.
/// The ordering it enforces is the normative part of §0.3, not an implementation detail.
#[derive(Default)]
pub(crate) struct Accumulator {
    effects: Mutex<Vec<EffectHandle>>,
}

impl Accumulator {
    pub(crate) fn push(&self, h: EffectHandle) {
        self.effects.lock().push(h);
    }
    /// Dispose every effect, last-registered first.
    pub(crate) async fn unwind(&self) {
        loop {
            let next = self.effects.lock().pop();
            match next {
                Some(h) => h.dispose().await,
                None => break,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{Context, KernelCore};
    use std::sync::atomic::AtomicUsize;

    fn trace() -> Arc<Mutex<Vec<&'static str>>> {
        Arc::new(Mutex::new(Vec::new()))
    }

    fn root() -> Context {
        Context::root(KernelCore::new())
    }

    #[tokio::test]
    async fn inverses_unwind_lifo() {
        let ctx = root();
        let t = trace();
        let (t1, t2) = (t.clone(), t.clone());
        let h = ctx
            .effect(move |e| async move {
                e.defer_sync(move || t1.lock().push("first"));
                e.defer_sync(move || t2.lock().push("second"));
                Ok(())
            })
            .await
            .unwrap();
        h.dispose().await;
        assert_eq!(&*t.lock(), &["second", "first"]);
    }

    #[tokio::test]
    async fn effects_unwind_lifo_within_a_fiber() {
        let ctx = root();
        let t = trace();
        for name in ["a", "b", "c"] {
            let t = t.clone();
            ctx.effect(move |e| async move {
                e.defer_sync(move || t.lock().push(name));
                Ok(())
            })
            .await
            .unwrap();
        }
        ctx.core().unwind_fiber(ctx.fiber_uid()).await;
        assert_eq!(&*t.lock(), &["c", "b", "a"]);
    }

    #[tokio::test]
    async fn disposer_fires_at_most_once() {
        let ctx = root();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let h = ctx
            .effect(move |e| async move {
                e.defer_sync(move || {
                    n2.fetch_add(1, Ordering::SeqCst);
                });
                Ok(())
            })
            .await
            .unwrap();
        h.dispose().await;
        h.dispose().await;
        h.clone().dispose().await;
        assert_eq!(n.load(Ordering::SeqCst), 1);
        assert!(h.is_disposed());
    }

    #[tokio::test]
    async fn concurrent_dispose_calls_await_one_run() {
        let ctx = root();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let h = ctx
            .effect(move |e| async move {
                e.defer(move || {
                    let n2 = n2.clone();
                    async move {
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                        n2.fetch_add(1, Ordering::SeqCst);
                    }
                });
                Ok(())
            })
            .await
            .unwrap();
        let calls: Vec<_> = (0..8)
            .map(|_| {
                let h = h.clone();
                tokio::spawn(async move { h.dispose().await })
            })
            .collect();
        for c in calls {
            c.await.unwrap();
        }
        assert_eq!(n.load(Ordering::SeqCst), 1);
        // Every caller observed the completed run, not just the one that claimed it.
        assert!(h.is_disposed());
    }

    #[tokio::test]
    async fn dispose_halts_in_flight_effect_at_yield() {
        let ctx = root();
        let halted = Arc::new(AtomicBool::new(false));
        let started = Arc::new(Notify::new());
        let (h_flag, s_flag) = (halted.clone(), started.clone());
        let h = ctx.effect_spawn(move |e| async move {
            s_flag.notify_waiters();
            loop {
                if e.checkpoint().await.is_err() {
                    h_flag.store(true, Ordering::SeqCst);
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        });
        // Let the body reach its loop.
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        h.dispose().await;
        assert!(halted.load(Ordering::SeqCst), "body was never halted");
    }

    #[tokio::test]
    async fn halted_effect_still_unwinds_what_it_deferred() {
        let ctx = root();
        let t = trace();
        let t1 = t.clone();
        let h = ctx.effect_spawn(move |e| async move {
            let t2 = t1.clone();
            e.defer_sync(move || t2.lock().push("early"));
            loop {
                if e.checkpoint().await.is_err() {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        });
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        h.dispose().await;
        assert_eq!(&*t.lock(), &["early"]);
    }
}
