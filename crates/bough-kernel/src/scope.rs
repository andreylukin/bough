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
use crate::event::{EmitEvent, ParallelEvent, SerialEvent, WaterfallEvent};

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
/// admitted only for dispatches targeted at `key` or a descendant; disposing the guard unwinds all
/// of them.
pub fn create_scope(ctx: &Context, key: ScopeKey) -> ScopeGuard {
    todo!("WP-2")
}

/// Owns the lifetime of everything registered through a scoped context.
pub struct ScopeGuard {
    _priv: (),
}

impl ScopeGuard {
    /// The scoped context to register through.
    pub fn context(&self) -> &Context {
        todo!("WP-2")
    }
    /// The key this scope is tagged with.
    pub fn key(&self) -> &ScopeKey {
        todo!("WP-2")
    }
    /// Disposing this unwinds every registration made through [`ScopeGuard::context`].
    pub fn effect(&self) -> &EffectHandle {
        todo!("WP-2")
    }
}

/// Route a dispatch to untagged listeners PLUS the subject's own PLUS its ancestors'.
pub fn scope_target<'a>(base: &'a Context, key: &ScopeKey) -> ScopedDispatch<'a> {
    todo!("WP-2")
}

/// The dispatch surface of [`scope_target`]; mirrors `Context`'s four dispatch calls.
pub struct ScopedDispatch<'a> {
    _marker: std::marker::PhantomData<&'a Context>,
}

impl ScopedDispatch<'_> {
    pub fn emit<E: EmitEvent>(&self, payload: E::Payload) {
        todo!("WP-2")
    }
    pub async fn parallel<E: ParallelEvent>(&self, payload: E::Payload) {
        todo!("WP-2")
    }
    pub async fn serial<E: SerialEvent>(&self, payload: E::Payload) -> Option<E::Output> {
        todo!("WP-2")
    }
    pub async fn waterfall<E: WaterfallEvent>(&self, value: E::Value) -> E::Value {
        todo!("WP-2")
    }
}
