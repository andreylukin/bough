//! Invariant: a dependent targets a *binding identity* (`ProviderUid`), never a value (§0.3).
//! Overwriting a value in place is therefore invisible to dependents; withdraw-and-re-provide is
//! not. Every provision is an effect of the providing fiber and is withdrawn on unload, LIFO,
//! before any other inverse of that fiber runs.

use std::any::Any;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};

use crate::config::RealmLabel;
use crate::context::KernelCore;
use crate::effect::EffectHandle;
use crate::fiber::FiberUid;
use crate::scope::ScopeKey;

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

/// One live binding in the store, as the kernel's own diagnostics see it.
#[derive(Clone)]
pub struct Binding {
    pub uid: ProviderUid,
    pub value: Arc<dyn Any + Send + Sync>,
}

/// Where one binding lives: its realm, the service NAME, and the scope it was provided through
/// (`None` = the untagged global binding).
pub(crate) type StoreKey = (RealmLabel, &'static str, Option<ScopeKey>);

/// Every live binding, behind one lock. No async work ever happens while it is held.
#[derive(Default)]
pub(crate) struct Store {
    map: RwLock<HashMap<StoreKey, Binding>>,
    seq: AtomicU64,
}

impl Store {
    pub(crate) fn next_seq(&self) -> u64 {
        self.seq.fetch_add(1, Ordering::SeqCst)
    }

    pub(crate) fn insert(&self, key: StoreKey, binding: Binding) {
        self.map.write().insert(key, binding);
    }

    /// Look a binding up by realm + NAME + scope where the caller holds a borrowed `name`. The
    /// store's keys carry the `&'static str` the `ServiceKey` supplied; equality is on content.
    pub(crate) fn get_by_name(
        &self,
        realm: &RealmLabel,
        name: &str,
        scope: Option<&ScopeKey>,
    ) -> Option<Binding> {
        self.map
            .read()
            .iter()
            .find(|(k, _)| k.0 == *realm && k.1 == name && k.2.as_ref() == scope)
            .map(|(_, b)| b.clone())
    }

    /// Remove `key` only if the binding there is still `uid`'s: a slot re-provided by someone else
    /// is not this provider's to withdraw.
    pub(crate) fn remove_if(&self, key: &StoreKey, uid: ProviderUid) -> bool {
        let mut map = self.map.write();
        match map.get(key) {
            Some(b) if b.uid == uid => {
                map.remove(key);
                true
            }
            _ => false,
        }
    }

    /// Remove every binding provided by `fiber`. The first step of UNLOADING (§0.3): a fiber stops
    /// providing before any of its inverses run.
    pub(crate) fn withdraw_fiber(&self, fiber: FiberUid) -> Vec<&'static str> {
        let mut map = self.map.write();
        let doomed: Vec<StoreKey> = map
            .iter()
            .filter(|(_, b)| b.uid.fiber == fiber)
            .map(|(k, _)| k.clone())
            .collect();
        let mut names = Vec::new();
        for k in doomed {
            names.push(k.1);
            map.remove(&k);
        }
        names
    }

    /// Whether `fiber` currently provides `name` in any realm or scope. A fiber may always read
    /// what it itself provides without declaring it in `inject`.
    pub(crate) fn fiber_provides(&self, fiber: FiberUid, name: &str) -> bool {
        self.map
            .read()
            .iter()
            .any(|(k, b)| k.1 == name && b.uid.fiber == fiber)
    }

    /// The service NAMEs `fiber` provides, for `RowSnapshot::provides`.
    pub(crate) fn provided_by(&self, fiber: FiberUid) -> Vec<&'static str> {
        let mut v: Vec<&'static str> = self
            .map
            .read()
            .iter()
            .filter(|(_, b)| b.uid.fiber == fiber)
            .map(|(k, _)| k.1)
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    pub(crate) fn len(&self) -> usize {
        self.map.read().len()
    }
}

/// Returned by [`crate::Context::provide`].
///
/// Dropping the slot does **not** withdraw the binding: the owning fiber's effect accumulator
/// does, LIFO, on unload.
pub struct ServiceSlot<K: ServiceKey> {
    pub(crate) core: Arc<KernelCore>,
    pub(crate) key: StoreKey,
    /// Shared with the provision's own disposer, so a `republish` retargets what `withdraw`
    /// removes rather than leaving the old identity behind.
    pub(crate) uid: Arc<Mutex<ProviderUid>>,
    pub(crate) effect: EffectHandle,
    pub(crate) _marker: std::marker::PhantomData<fn() -> K>,
}

impl<K: ServiceKey> ServiceSlot<K> {
    /// This binding's identity.
    pub fn uid(&self) -> ProviderUid {
        *self.uid.lock()
    }
    /// The effect that owns the provision; disposing it withdraws.
    pub fn effect(&self) -> &EffectHandle {
        &self.effect
    }
    /// Overwrite the value in place. Same `ProviderUid`; dependents are NOT notified (§0.3).
    pub fn set(&self, value: K::Value) {
        let uid = self.uid();
        self.core.store.insert(
            self.key.clone(),
            Binding {
                uid,
                value: Arc::new(value),
            },
        );
    }
    /// Withdraw and re-provide: a new `seq`, so dependents recompute and reload (§0.3).
    pub async fn republish(&self, value: K::Value) {
        let old = self.uid();
        self.core.store.remove_if(&self.key, old);
        let uid = ProviderUid {
            fiber: old.fiber,
            seq: self.core.store.next_seq(),
        };
        *self.uid.lock() = uid;
        self.core.store.insert(
            self.key.clone(),
            Binding {
                uid,
                value: Arc::new(value),
            },
        );
    }
    /// Withdraw now. Idempotent.
    pub async fn withdraw(&self) {
        self.effect.dispose().await;
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::Inject;
    use crate::context::{CommittedView, Context, KernelCore};
    use crate::error::KernelError;
    use crate::fiber::EntryId;

    pub(crate) struct Greeting;
    impl ServiceKey for Greeting {
        type Value = String;
        const NAME: &'static str = "greeting";
    }

    pub(crate) struct Ledger;
    impl ServiceKey for Ledger {
        type Value = u32;
        const NAME: &'static str = "ledger";
    }

    fn kernel() -> Arc<KernelCore> {
        KernelCore::new()
    }

    /// A row context: its own fiber, its own row id, its own declared inject set.
    pub(crate) fn row(
        core: &Arc<KernelCore>,
        id: &str,
        plugin: &'static str,
        inject: Inject,
    ) -> Context {
        Context::root(core.clone()).for_row(core.new_fiber_uid(), EntryId::new(id), plugin, inject)
    }

    #[tokio::test]
    async fn provide_binds_and_dispose_withdraws() {
        let core = kernel();
        let provider = row(&core, "greeting-echo", "greeting-echo", Inject::none());
        let consumer = row(&core, "hello", "hello", Inject::required(["greeting"]));

        let slot = provider.provide::<Greeting>("hi".into()).await.unwrap();
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "hi");

        slot.effect().dispose().await;
        assert!(matches!(
            consumer.get::<Greeting>(),
            Err(KernelError::ServiceUnavailable { .. })
        ));
        assert_eq!(core.store.len(), 0);
    }

    #[tokio::test]
    async fn undeclared_key_errors_at_point_of_use() {
        let core = kernel();
        let provider = row(&core, "greeting-echo", "greeting-echo", Inject::none());
        provider.provide::<Greeting>("hi".into()).await.unwrap();

        // Declares `greeting`, reads `ledger`. The key is not bound at all...
        let consumer = row(&core, "hello", "hello", Inject::required(["greeting"]));
        assert!(matches!(
            consumer.get::<Ledger>(),
            Err(KernelError::UndeclaredService { .. })
        ));

        // ...and it is STILL UndeclaredService when the key happens to be bound.
        let other = row(&core, "ledger-row", "ledger-sqlite", Inject::none());
        other.provide::<Ledger>(7).await.unwrap();
        assert!(matches!(
            consumer.get::<Ledger>(),
            Err(KernelError::UndeclaredService { .. })
        ));
    }

    #[tokio::test]
    async fn undeclared_key_error_names_key_and_plugin() {
        let core = kernel();
        let consumer = row(&core, "hello.greeter", "hello", Inject::none());
        let err = consumer.get::<Ledger>().unwrap_err();
        assert_eq!(
            err.to_string(),
            "plugin `hello` (row `hello.greeter`) read service `ledger` without declaring it in inject"
        );
    }

    #[tokio::test]
    async fn committed_view_survives_a_later_rebind() {
        let core = kernel();
        let provider = row(&core, "greeting-echo", "greeting-echo", Inject::none());
        let slot = provider.provide::<Greeting>("first".into()).await.unwrap();

        let consumer =
            row(&core, "hello", "hello", Inject::required(["greeting"])).with_view(Arc::new(
                CommittedView::capture(&core, &["greeting"], &Default::default(), None),
            ));
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "first");

        slot.republish("second".into()).await;
        // The live store moved on; the fiber's committed view did not (§0.3).
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "first");
        assert_eq!(*consumer.peek_live::<Greeting>().unwrap(), "second");
    }

    #[tokio::test]
    async fn set_in_place_keeps_the_provider_uid() {
        let core = kernel();
        let provider = row(&core, "greeting-echo", "greeting-echo", Inject::none());
        let slot = provider.provide::<Greeting>("one".into()).await.unwrap();
        let before = slot.uid();
        slot.set("two".into());
        assert_eq!(slot.uid(), before);
        let consumer = row(&core, "hello", "hello", Inject::required(["greeting"]));
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "two");
    }

    #[tokio::test]
    async fn republish_bumps_the_provider_uid() {
        let core = kernel();
        let provider = row(&core, "greeting-echo", "greeting-echo", Inject::none());
        let slot = provider.provide::<Greeting>("one".into()).await.unwrap();
        let before = slot.uid();
        slot.republish("one".into()).await;
        let after = slot.uid();
        assert_eq!(before.fiber, after.fiber);
        assert_ne!(before.seq, after.seq, "republish must change the identity");
        // Same fiber, equal value: the change is in the identity alone.
        let consumer = row(&core, "hello", "hello", Inject::required(["greeting"]));
        assert_eq!(*consumer.get::<Greeting>().unwrap(), "one");
    }
}
