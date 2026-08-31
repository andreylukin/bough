//! Invariant: two directions, deliberately opposite (§0.3). Views inherit DOWN — resolving a key
//! from a context tagged `a/b/c` tries `a/b/c`, then `a/b`, then `a`, then the untagged global
//! binding, nearest shadowing farthest. Admission extends UP — a dispatch targeted at `a/b/c`
//! reaches listeners tagged `a/b/c`, `a/b`, `a` and untagged ones, but never a descendant
//! `a/b/c/d` nor a sibling `a/b/x`.
//!
//! Scopes route trusted in-process plugins. They are not sandboxes and not authority boundaries.

use std::sync::Arc;

use crate::context::Context;
use crate::effect::EffectHandle;
use crate::event::{self, EmitEvent, ParallelEvent, SerialEvent, WaterfallEvent};

/// A scope path. Cheap to clone; equality and hashing cover the whole parent chain.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ScopeKey {
    id: Arc<str>,
    parent: Option<Arc<ScopeKey>>,
}

impl ScopeKey {
    /// A root scope.
    pub fn new(id: impl Into<Arc<str>>) -> Self {
        Self {
            id: id.into(),
            parent: None,
        }
    }
    /// A child of this scope.
    pub fn child(&self, id: impl Into<Arc<str>>) -> Self {
        Self {
            id: id.into(),
            parent: Some(Arc::new(self.clone())),
        }
    }
    /// `self` first, then each ancestor up to the root.
    pub fn ancestors(&self) -> impl Iterator<Item = &ScopeKey> {
        std::iter::successors(Some(self), |s| s.parent.as_deref())
    }
    /// The last path segment.
    pub fn id(&self) -> &str {
        &self.id
    }
}

/// Tag `ctx` with `key`.
///
/// Registrations made through the returned context are scope-VISIBLE and scope-LIFETIME: services
/// provided through it bind only for `key` and its descendants; listeners registered through it are
/// admitted only for dispatches targeted at `key` or a descendant; disposing `guard.effect()`
/// unwinds all of them.
///
/// NOT an RAII guard despite the name: dropping it does nothing. The scope is an effect of the
/// OWNING FIBER (`push_fiber_effect` below), so its registrations live until either
/// `guard.effect().dispose()` is awaited or the fiber unwinds — which is what makes disposal
/// awaited rather than "kills issued but not awaited" (§0.2). `Drop` could only spawn.
pub fn create_scope(ctx: &Context, key: ScopeKey) -> ScopeGuard {
    let (handle, _inner) = EffectHandle::new();
    // The scope itself is an effect of the fiber, so unloading the fiber unwinds the scope, which
    // unwinds everything registered through it.
    ctx.core()
        .push_fiber_effect(ctx.fiber_uid(), handle.clone());
    let scoped = ctx.with_scope(key.clone(), handle.clone());
    ScopeGuard {
        context: scoped,
        key,
        effect: handle,
    }
}

/// Owns the lifetime of everything registered through a scoped context.
///
/// Dropping it is a no-op; see [`create_scope`].
pub struct ScopeGuard {
    context: Context,
    key: ScopeKey,
    effect: EffectHandle,
}

impl ScopeGuard {
    /// The scoped context to register through.
    pub fn context(&self) -> &Context {
        &self.context
    }
    /// The key this scope is tagged with.
    pub fn key(&self) -> &ScopeKey {
        &self.key
    }
    /// Disposing this unwinds every registration made through [`ScopeGuard::context`].
    pub fn effect(&self) -> &EffectHandle {
        &self.effect
    }
}

/// Route a dispatch to untagged listeners PLUS the subject's own PLUS its ancestors'.
pub fn scope_target<'a>(base: &'a Context, key: &ScopeKey) -> ScopedDispatch<'a> {
    ScopedDispatch {
        base,
        target: key.clone(),
    }
}

/// The dispatch surface of [`scope_target`]; mirrors `Context`'s four dispatch calls.
pub struct ScopedDispatch<'a> {
    base: &'a Context,
    target: ScopeKey,
}

impl ScopedDispatch<'_> {
    pub fn emit<E: EmitEvent>(&self, payload: E::Payload) {
        event::emit_ev::<E>(self.base.core(), payload, Some(self.target.clone()));
    }
    pub async fn parallel<E: ParallelEvent>(&self, payload: E::Payload) {
        event::parallel_ev::<E>(self.base.core(), payload, Some(self.target.clone())).await
    }
    pub async fn serial<E: SerialEvent>(&self, payload: E::Payload) -> Option<E::Output> {
        event::serial_ev::<E>(self.base.core(), payload, Some(self.target.clone())).await
    }
    pub async fn waterfall<E: WaterfallEvent>(&self, value: E::Value) -> E::Value {
        event::waterfall_ev::<E>(self.base.core(), value, Some(self.target.clone())).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Inject;
    use crate::context::KernelCore;
    use crate::service::tests::{row, Greeting, Ledger};
    use parking_lot::Mutex;
    use std::time::Duration;

    struct Note;
    impl crate::event::ParallelEvent for Note {
        const NAME: &'static str = "test/note";
        type Payload = ();
    }

    type Trace = Arc<Mutex<Vec<&'static str>>>;
    fn trace() -> Trace {
        Arc::new(Mutex::new(Vec::new()))
    }

    #[tokio::test]
    async fn scoped_service_shadows_global_for_that_key_only() {
        let core = KernelCore::new();
        let global = row(&core, "global", "p", Inject::none());
        global.provide::<Greeting>("global".into()).await.unwrap();
        global.provide::<Ledger>(1).await.unwrap();

        let consumer = row(
            &core,
            "consumer",
            "q",
            Inject::required(["greeting", "ledger"]),
        );
        let session = create_scope(&consumer, ScopeKey::new("session-1"));
        session
            .context()
            .provide::<Greeting>("scoped".into())
            .await
            .unwrap();

        // Nearest shadows farthest for `greeting`...
        assert_eq!(*session.context().get::<Greeting>().unwrap(), "scoped");
        // ...and `ledger`, which the scope never rebound, still resolves to the global binding.
        assert_eq!(*session.context().get::<Ledger>().unwrap(), 1);
        // The untagged context is unaffected.
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "global");
    }

    #[tokio::test]
    async fn scoped_view_inherits_down_parent_chain() {
        let core = KernelCore::new();
        let ctx = row(&core, "row", "p", Inject::required(["greeting"]));
        let a = ScopeKey::new("a");
        let ab = a.child("b");
        let abc = ab.child("c");

        let outer = create_scope(&ctx, a.clone());
        outer
            .context()
            .provide::<Greeting>("from-a".into())
            .await
            .unwrap();

        // A context tagged a/b/c finds nothing at a/b/c or a/b, and inherits a's binding.
        let deep = create_scope(&ctx, abc.clone());
        assert_eq!(*deep.context().get::<Greeting>().unwrap(), "from-a");

        // Bind at a/b: now the nearer one shadows a's.
        let mid = create_scope(&ctx, ab.clone());
        mid.context()
            .provide::<Greeting>("from-a-b".into())
            .await
            .unwrap();
        let deep2 = create_scope(&ctx, abc.clone());
        assert_eq!(*deep2.context().get::<Greeting>().unwrap(), "from-a-b");
    }

    #[tokio::test]
    async fn scoped_dispatch_admission_extends_up() {
        let core = KernelCore::new();
        let ctx = row(&core, "row", "p", Inject::none());
        let a = ScopeKey::new("a");
        let ab = a.child("b");

        let t = trace();
        {
            let t = t.clone();
            ctx.on_parallel::<Note, _, _>(move |()| {
                let t = t.clone();
                async move { t.lock().push("untagged") }
            })
            .await
            .unwrap();
        }
        let sa = create_scope(&ctx, a.clone());
        {
            let t = t.clone();
            sa.context()
                .on_parallel::<Note, _, _>(move |()| {
                    let t = t.clone();
                    async move { t.lock().push("a") }
                })
                .await
                .unwrap();
        }
        let sab = create_scope(&ctx, ab.clone());
        {
            let t = t.clone();
            sab.context()
                .on_parallel::<Note, _, _>(move |()| {
                    let t = t.clone();
                    async move { t.lock().push("a/b") }
                })
                .await
                .unwrap();
        }

        scope_target(&ctx, &ab).parallel::<Note>(()).await;
        let mut seen = t.lock().clone();
        seen.sort_unstable();
        assert_eq!(seen, ["a", "a/b", "untagged"]);
    }

    #[tokio::test]
    async fn scoped_dispatch_skips_sibling_and_descendant_scopes() {
        let core = KernelCore::new();
        let ctx = row(&core, "row", "p", Inject::none());
        let a = ScopeKey::new("a");
        let ab = a.child("b");
        let abx = ab.child("x"); // descendant of the target
        let ay = a.child("y"); // sibling of the target

        let t = trace();
        for (key, label) in [(abx, "descendant"), (ay, "sibling")] {
            let s = create_scope(&ctx, key);
            let t = t.clone();
            s.context()
                .on_parallel::<Note, _, _>(move |()| {
                    let t = t.clone();
                    async move { t.lock().push(label) }
                })
                .await
                .unwrap();
            // `ScopeGuard` has no `Drop`, so this only makes the intent explicit: the scope
            // stays registered, owned by the fiber, for the rest of the test.
            std::mem::forget(s);
        }

        scope_target(&ctx, &ab).parallel::<Note>(()).await;
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert!(t.lock().is_empty(), "delivered to {:?}", t.lock());
    }

    #[tokio::test]
    async fn disposing_a_scope_unwinds_its_registrations() {
        let core = KernelCore::new();
        let ctx = row(&core, "row", "p", Inject::required(["greeting"]));
        let key = ScopeKey::new("session-1");
        let s = create_scope(&ctx, key.clone());
        s.context()
            .provide::<Greeting>("scoped".into())
            .await
            .unwrap();
        s.context()
            .on_parallel::<Note, _, _>(|()| async move {})
            .await
            .unwrap();
        assert_eq!(core.binding_count(), 1);
        assert_eq!(core.listener_count(Note::NAME), 1);

        s.effect().dispose().await;

        assert_eq!(
            core.binding_count(),
            0,
            "a scoped binding outlived its scope"
        );
        assert_eq!(
            core.listener_count(Note::NAME),
            0,
            "a scoped listener outlived its scope"
        );
        assert!(matches!(
            s.context().get::<Greeting>(),
            Err(crate::error::KernelError::ServiceUnavailable { .. })
        ));
    }
}
