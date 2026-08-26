//! Invariant: the kernel handle owns the running tree and is the only writer of it. A live
//! recompose that fails leaves the last good tree untouched and has already broadcast
//! `config-update-failed` (§0.3); `shutdown` unloads everything, LIFO, awaited, so a caller can
//! restore a terminal after it returns (§0.1 item 2, teardown-before-exit).
//!
//! The tree is never mutated by walking a diff and calling lifecycle methods. The diff writes
//! fiber TARGETS (`reconcile.rs`) and the drivers converge (`fiber.rs`); that is what makes the
//! quiescent state a function of the final tree alone.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use crate::catalog::Catalog;
use crate::config::{Composition, Entry, Fingerprint, Inject, RealmLabel};
use crate::context::{CommittedView, Context, KernelCore};
use crate::error::{ComposeError, KernelError, PluginError};
use crate::event::FiberStateChange;
use crate::fiber::{
    quiesce_runtime, EntryId, Fiber, FiberBody, FiberHandle, FiberRuntime, FiberState, FiberUid,
    ResolvedTargets, Resolver, StateSink,
};
use crate::invariant::{CheckHost, InvariantRunner, InvariantSpec, InvariantViolation};
use crate::plugin::{ErasedConfig, ErasedPlugin, Reconfigure};
use crate::reconcile::{diff_trees, flatten, is_disabled, parents, TargetWrite};

/// Construction-time knobs. Not a plugin config: these are the kernel's own, set by the launcher
/// from the profile.
pub struct KernelOptions {
    /// Profile name, visible to `!!expr`'s `profile()`.
    pub profile: String,
    /// Create the invariant runner (`dev` and the test harness; false in `tui`/`headless`).
    pub invariants: bool,
    /// How long the reconciler coalesces target writes before converging.
    pub reconcile_debounce: Duration,
}

impl Default for KernelOptions {
    fn default() -> Self {
        KernelOptions {
            profile: "dev".to_string(),
            invariants: false,
            reconcile_debounce: Duration::from_millis(50),
        }
    }
}

/// Where the kernel's own events go.
///
/// A seam, deliberately: dispatch belongs to `Context` (WP-2), and the lifecycle must not need a
/// context of its own to report a transition.
pub trait KernelEvents: Send + Sync + 'static {
    fn fiber_state(&self, _change: FiberStateChange) {}
    fn config_update_failed(&self, _error: Arc<ComposeError>) {}
    fn config_updated(&self, _fingerprint: Fingerprint) {}
    fn invariant_violated(&self, _violation: Arc<InvariantViolation>) {}
}

/// Drops everything. Used until a root `Context` exists to dispatch through.
pub struct SilentEvents;
impl KernelEvents for SilentEvents {}

struct EventsAsSink(Arc<dyn KernelEvents>);
impl StateSink for EventsAsSink {
    fn fiber_state(&self, change: FiberStateChange) {
        self.0.fiber_state(change);
    }
}

/// Builds the `FiberBody` for a row.
///
/// The kernel's own implementation wraps `ErasedPlugin` + a `Context`; the tests use a recording
/// factory, which is how the reconciliation table is asserted without a plugin crate.
pub trait BodyFactory: Send + Sync + 'static {
    /// Parse + validate the row and produce its body. `Err` here rejects the whole candidate tree
    /// BEFORE anything is touched — that is what keeps the last good tree running (§0.3).
    fn build(&self, entry: &Entry) -> Result<Arc<dyn FiberBody>, ComposeError>;
    /// Hand the new config to the plugin. Always called on a config diff, material or not.
    fn reconfigure(
        &self,
        current: &Arc<dyn FiberBody>,
        old: &Entry,
        new: &Entry,
    ) -> Result<(Arc<dyn FiberBody>, Reconfigure), ComposeError>;
    /// The catalog name as a `'static` string, for the snapshot.
    fn static_name(&self, plugin: &str) -> Option<&'static str>;
}

struct Row {
    entry: Entry,
    uid: FiberUid,
}

/// The running tree.
pub struct Kernel {
    core: Arc<KernelCore>,
    root: Mutex<Context>,
    catalog: Option<Arc<Catalog>>,
    options: KernelOptions,
    rt: Arc<FiberRuntime>,
    factory: Arc<dyn BodyFactory>,
    events: Arc<dyn KernelEvents>,
    tree: Mutex<Vec<Entry>>,
    rows: Mutex<BTreeMap<EntryId, Row>>,
    composition: Mutex<Option<Arc<Composition>>>,
    runner: Mutex<Option<Arc<InvariantRunner>>>,
    updates: AtomicU64,
}

impl Kernel {
    /// Build a kernel over a catalog. Nothing is mounted until [`Kernel::load`].
    ///
    /// The plugin-backed factory and the live binding-store resolver sit on WP-2's
    /// `context::KernelCore`: the binding store, the per-fiber effect accumulator and the root
    /// `Context`. The `e2e` tests in this module drive this constructor end to end.
    pub fn new(catalog: Catalog, options: KernelOptions) -> Arc<Kernel> {
        let core = KernelCore::new();
        let root = Context::root(core.clone());
        let catalog = Arc::new(catalog);
        let events: Arc<dyn KernelEvents> = Arc::new(ContextEvents { ctx: root.clone() });
        let factory: Arc<dyn BodyFactory> = Arc::new(PluginFactory {
            catalog: catalog.clone(),
            core: core.clone(),
            root: root.clone(),
        });
        let resolver: Arc<dyn Resolver> = Arc::new(CoreResolver { core: core.clone() });
        Kernel::assemble(
            core,
            root,
            Some(catalog),
            options,
            factory,
            resolver,
            events,
        )
    }

    /// The composable constructor. `catalog` is `None` only for the in-crate tests, which supply
    /// their own factory.
    pub fn with_parts(
        catalog: Option<Catalog>,
        options: KernelOptions,
        factory: Arc<dyn BodyFactory>,
        resolver: Arc<dyn Resolver>,
        events: Arc<dyn KernelEvents>,
    ) -> Arc<Kernel> {
        let core = KernelCore::new();
        let root = Context::root(core.clone());
        Kernel::assemble(
            core,
            root,
            catalog.map(Arc::new),
            options,
            factory,
            resolver,
            events,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn assemble(
        core: Arc<KernelCore>,
        root: Context,
        catalog: Option<Arc<Catalog>>,
        options: KernelOptions,
        factory: Arc<dyn BodyFactory>,
        resolver: Arc<dyn Resolver>,
        events: Arc<dyn KernelEvents>,
    ) -> Arc<Kernel> {
        let rt = FiberRuntime::new(resolver, Arc::new(EventsAsSink(events.clone())));
        let kernel = Arc::new(Kernel {
            core,
            root: Mutex::new(root),
            catalog,
            options,
            rt,
            factory,
            events,
            tree: Mutex::new(Vec::new()),
            rows: Mutex::new(BTreeMap::new()),
            composition: Mutex::new(None),
            runner: Mutex::new(None),
            updates: AtomicU64::new(0),
        });
        // The root context carries the kernel handle, so a plugin's `ctx.kernel()` works.
        let with_kernel = kernel.root.lock().with_kernel(kernel.clone());
        *kernel.root.lock() = with_kernel;
        // The runner is a property of the profile, not of a caller remembering to ask for it: if
        // `KernelOptions::invariants` is on, it exists from the moment the kernel does (§0.2).
        kernel.start_invariants();
        kernel
    }

    /// The root context: the parent of every top-level row.
    pub fn root(&self) -> Context {
        self.root.lock().clone()
    }

    /// The shared state every context in this kernel reads.
    pub fn core(&self) -> &Arc<KernelCore> {
        &self.core
    }

    /// The catalog this kernel resolves plugin names against.
    pub fn catalog(&self) -> Option<Arc<Catalog>> {
        self.catalog.clone()
    }

    pub fn options(&self) -> &KernelOptions {
        &self.options
    }

    /// The fiber runtime, for the launcher's diagnostics and for tests.
    pub fn runtime(&self) -> &Arc<FiberRuntime> {
        &self.rt
    }

    /// Mount a composition for the first time.
    pub async fn load(&self, c: Composition) -> Result<(), KernelError> {
        self.apply_composition(c).await
    }

    /// Live recompose. On `Err` the last good tree is untouched and `config-update-failed` has
    /// already been emitted (§0.3).
    pub async fn update(&self, c: Composition) -> Result<(), KernelError> {
        self.apply_composition(c).await
    }

    async fn apply_composition(&self, c: Composition) -> Result<(), KernelError> {
        let fingerprint = c.fingerprint.clone();
        self.update_tree(c.tree.clone()).await?;
        *self.composition.lock() = Some(Arc::new(c));
        self.events.config_updated(fingerprint);
        Ok(())
    }

    /// Reconcile the running tree towards `new`, then converge.
    ///
    /// Every row is validated FIRST; a single bad row rejects the whole candidate and nothing is
    /// touched. This is the update-failure path §0.3 mandates, and it is why the launcher can keep
    /// watching a broken patch file without losing the tree.
    pub async fn update_tree(&self, new: Vec<Entry>) -> Result<(), KernelError> {
        let old = self.tree.lock().clone();
        let writes = diff_trees(&old, &new);
        let new_rows: BTreeMap<EntryId, Entry> = flatten(&new)
            .into_iter()
            .map(|(id, e)| (id, e.clone()))
            .collect();

        // ---- validate the whole candidate before touching anything --------
        let mut built: BTreeMap<EntryId, Arc<dyn FiberBody>> = BTreeMap::new();
        for w in &writes {
            let id = w.id();
            let Some(entry) = new_rows.get(id) else {
                continue;
            };
            let needs_body = matches!(
                w,
                TargetWrite::Create { .. }
                    | TargetWrite::Rebuild { .. }
                    | TargetWrite::Reload { .. }
                    | TargetWrite::Retarget { .. }
            );
            if needs_body && !built.contains_key(id) {
                match self.factory.build(entry) {
                    Ok(b) => {
                        built.insert(id.clone(), b);
                    }
                    Err(e) => {
                        let e = Arc::new(e);
                        self.events.config_update_failed(e.clone());
                        return Err(KernelError::Compose(Arc::try_unwrap(e).unwrap_or_else(
                            |a| ComposeError::BadYaml {
                                layer: crate::config::LayerId::new("candidate"),
                                detail: a.to_string(),
                            },
                        )));
                    }
                }
            }
        }

        // ---- apply the target writes --------------------------------------
        for w in &writes {
            match w {
                TargetWrite::Create { id } => {
                    let Some(entry) = new_rows.get(id) else {
                        continue;
                    };
                    let body = match built.get(id) {
                        Some(b) => b.clone(),
                        None => continue,
                    };
                    self.create_row(entry, &new, body);
                }
                TargetWrite::Rebuild { id } | TargetWrite::Reload { id } => {
                    let Some(entry) = new_rows.get(id) else {
                        continue;
                    };
                    let Some(body) = built.get(id).cloned() else {
                        continue;
                    };
                    if let Some(uid) = self.uid_of(id) {
                        self.rt.dispose(uid).await;
                        self.rows.lock().remove(id);
                    }
                    self.create_row(entry, &new, body);
                }
                TargetWrite::Unload { id } => {
                    if let Some(f) = self.fiber_of(id) {
                        f.set_want(false);
                    }
                }
                TargetWrite::Load { id } => {
                    if let Some(f) = self.fiber_of(id) {
                        f.set_want(true);
                    }
                }
                TargetWrite::Retarget { id } => {
                    let Some(body) = built.get(id).cloned() else {
                        continue;
                    };
                    if let Some(f) = self.fiber_of(id) {
                        let before = f.view();
                        // The row keeps its fiber across a retarget, so the new body takes over
                        // the running fiber's context rather than starting without one.
                        body.attach(f.uid());
                        f.set_body(body);
                        let (after, _unmet) = self.rt.resolve_view(&f);
                        // Reload IFF a resolved ProviderUid actually moved (§0.3): a declaration
                        // that resolves the same way is not a reason to restart a plugin.
                        let moved = match before {
                            Some(b) => !same_targets(&b, &after),
                            None => true,
                        };
                        if moved {
                            f.request_reload();
                        }
                    }
                }
                TargetWrite::Reconfigure { id } => {
                    let Some(entry) = new_rows.get(id) else {
                        continue;
                    };
                    let old_entry = self.entry_of(id);
                    let (Some(f), Some(old_entry)) = (self.fiber_of(id), old_entry) else {
                        continue;
                    };
                    let current = f.body();
                    match self.factory.reconfigure(&current, &old_entry, entry) {
                        Ok((body, verdict)) => {
                            f.set_body(body);
                            if verdict == Reconfigure::Reload {
                                f.request_reload();
                            }
                        }
                        Err(e) => {
                            // Validation already passed for creations; a reconfigure that still
                            // fails leaves the fiber alone and is reported.
                            let e = Arc::new(e);
                            self.events.config_update_failed(e.clone());
                            return Err(KernelError::Compose(ComposeError::BadYaml {
                                layer: crate::config::LayerId::new("candidate"),
                                detail: e.to_string(),
                            }));
                        }
                    }
                }
            }
        }

        // ---- rows that left the tree --------------------------------------
        let gone: Vec<EntryId> = self
            .rows
            .lock()
            .keys()
            .filter(|id| !new_rows.contains_key(*id))
            .cloned()
            .collect();
        for id in gone {
            if let Some(uid) = self.uid_of(&id) {
                self.rt.dispose(uid).await;
            }
            self.rows.lock().remove(&id);
        }

        // ---- record the new tree ------------------------------------------
        {
            let mut rows = self.rows.lock();
            for (id, entry) in &new_rows {
                if let Some(r) = rows.get_mut(id) {
                    r.entry = entry.clone();
                }
            }
        }
        *self.tree.lock() = new;
        self.updates.fetch_add(1, Ordering::SeqCst);

        self.quiesce().await;
        // A cascade may have disposed a child fiber whose row is still in the tree; drop the
        // bookkeeping for anything the runtime no longer holds.
        let stale: Vec<EntryId> = self
            .rows
            .lock()
            .iter()
            .filter(|(_, r)| self.rt.get(r.uid).is_none())
            .map(|(id, _)| id.clone())
            .collect();
        for id in stale {
            self.rows.lock().remove(&id);
        }
        if let Some(runner) = self.runner.lock().as_ref() {
            runner.collect_specs(self.active_plugin_specs());
        }
        self.run_quiesce_invariants().await;
        Ok(())
    }

    fn create_row(&self, entry: &Entry, tree: &[Entry], body: Arc<dyn FiberBody>) {
        let parent_id = parents(tree)
            .into_iter()
            .find(|(id, _)| id == &entry.id)
            .and_then(|(_, p)| p);
        let parent_uid = parent_id.and_then(|p| self.uid_of(&p));
        let plugin = entry
            .plugin
            .as_deref()
            .and_then(|p| self.factory.static_name(p));
        let realms: BTreeMap<String, RealmLabel> = entry.isolate.clone();
        let handle = self.rt.create(
            entry.id.clone(),
            plugin,
            realms,
            parent_uid,
            body,
            !is_disabled(entry),
        );
        self.rows.lock().insert(
            entry.id.clone(),
            Row {
                entry: entry.clone(),
                uid: handle.uid(),
            },
        );
    }

    fn uid_of(&self, id: &EntryId) -> Option<FiberUid> {
        self.rows.lock().get(id).map(|r| r.uid)
    }
    fn entry_of(&self, id: &EntryId) -> Option<Entry> {
        self.rows.lock().get(id).map(|r| r.entry.clone())
    }
    fn fiber_of(&self, id: &EntryId) -> Option<Arc<Fiber>> {
        self.uid_of(id).and_then(|u| self.rt.get(u))
    }

    /// The fiber currently holding a row, if any.
    pub fn fiber(&self, id: &EntryId) -> Option<FiberHandle> {
        self.uid_of(id).and_then(|u| self.rt.handle(u))
    }

    /// Return once no fiber is Loading or Unloading and no reconcile is pending — including
    /// fibers that a transition itself created. The workhorse of every test.
    pub async fn quiesce(&self) {
        quiesce_runtime(&self.rt).await;
    }

    /// The structural view tests assert on.
    pub fn snapshot(&self) -> TreeSnapshot {
        TreeSnapshot {
            fingerprint: self
                .composition
                .lock()
                .as_ref()
                .map(|c| c.fingerprint.clone())
                .unwrap_or_else(|| Fingerprint::of(&self.tree.lock())),
            rows: self.rows_snapshot(),
        }
    }

    /// The rows of [`Kernel::snapshot`], without needing a composition to have been loaded.
    pub fn rows_snapshot(&self) -> Vec<RowSnapshot> {
        let tree = self.tree.lock().clone();
        tree.iter().map(|e| self.row_snapshot(e)).collect()
    }

    fn row_snapshot(&self, e: &Entry) -> RowSnapshot {
        let fiber = self.fiber_of(&e.id);
        RowSnapshot {
            id: e.id.clone(),
            plugin: e.plugin.clone(),
            uid: fiber.as_ref().map(|f| f.uid()),
            state: fiber
                .as_ref()
                .map(|f| f.state())
                .unwrap_or(FiberState::Inactive),
            disabled: is_disabled(e),
            unmet: fiber.as_ref().map(|f| f.unmet()).unwrap_or_default(),
            provides: fiber.as_ref().map(|f| f.provides()).unwrap_or_default(),
            realms: e.isolate.clone(),
            children: {
                let mut kids: Vec<RowSnapshot> =
                    e.group.iter().map(|c| self.row_snapshot(c)).collect();
                // Nested mounts (`ctx.mount`) are runtime children: they are in no config tree,
                // but they ARE part of the structure a test asserts on, so the snapshot shows them
                // under the fiber that mounted them.
                if let Some(f) = fiber.as_ref().and_then(|f| self.rt.get(f.uid())) {
                    for child in f.children() {
                        if let Some(row) = self.fiber_snapshot(child) {
                            if !kids.iter().any(|k| k.id == row.id) {
                                kids.push(row);
                            }
                        }
                    }
                }
                kids
            },
        }
    }

    /// A row that exists only as a fiber: a nested mount, and its own nested mounts.
    fn fiber_snapshot(&self, uid: FiberUid) -> Option<RowSnapshot> {
        let f = self.rt.get(uid)?;
        Some(RowSnapshot {
            id: f.id().clone(),
            plugin: f.plugin().map(str::to_string),
            uid: Some(uid),
            state: f.state(),
            disabled: false,
            unmet: f.unmet(),
            provides: f.provides(),
            realms: f.realms(),
            children: f
                .children()
                .into_iter()
                .filter_map(|c| self.fiber_snapshot(c))
                .collect(),
        })
    }

    /// The composition currently live.
    pub fn composition(&self) -> Arc<Composition> {
        self.composition
            .lock()
            .clone()
            .expect("no composition loaded")
    }

    /// Violations recorded by the invariant runner; empty when it is not running.
    pub fn violations(&self) -> Vec<InvariantViolation> {
        self.runner
            .lock()
            .as_ref()
            .map(|r| r.violations())
            .unwrap_or_default()
    }

    /// Start the invariant runner. A no-op unless `KernelOptions::invariants` (§0.2, §2.9).
    pub fn start_invariants(self: &Arc<Self>) {
        if !self.options.invariants {
            return;
        }
        let mut slot = self.runner.lock();
        if slot.is_none() {
            *slot = Some(Arc::new(InvariantRunner::with_host(
                self.events.clone(),
                Arc::new(RowCheckHost {
                    kernel: Arc::downgrade(self),
                }),
            )));
        }
        if let Some(r) = slot.as_ref() {
            r.collect_specs(self.active_plugin_specs());
        }
    }

    fn active_plugin_specs(&self) -> Vec<(EntryId, crate::invariant::InvariantSpec)> {
        let mut out = Vec::new();
        let Some(catalog) = self.catalog.clone() else {
            return out;
        };
        for (id, row) in self.rows.lock().iter() {
            let Some(f) = self.rt.get(row.uid) else {
                continue;
            };
            if f.state() != FiberState::Active {
                continue;
            }
            let Some(name) = row.entry.plugin.as_deref() else {
                continue;
            };
            if let Some(p) = catalog.get(name) {
                for spec in p.invariants() {
                    out.push((id.clone(), spec));
                }
            }
        }
        out
    }

    async fn run_quiesce_invariants(&self) {
        let runner = {
            let r = self.runner.lock();
            r.clone()
        };
        if let Some(runner) = runner {
            runner.run_on_quiesce().await;
        }
    }

    /// Mount one row as a child of `parent`. The child is an effect of the parent fiber: the
    /// parent's teardown cascades to it (§0.3). A nested mount is not a config-tree row, but it
    /// does appear in [`Kernel::snapshot`] under the row that mounted it.
    pub async fn mount_child(
        &self,
        parent: FiberUid,
        entry: Entry,
    ) -> Result<FiberHandle, KernelError> {
        let body = self.factory.build(&entry).map_err(KernelError::Compose)?;
        let plugin = entry
            .plugin
            .as_deref()
            .and_then(|p| self.factory.static_name(p));
        let handle = self.rt.create(
            entry.id.clone(),
            plugin,
            entry.isolate.clone(),
            Some(parent),
            body,
            !is_disabled(&entry),
        );
        handle.settled().await;
        Ok(handle)
    }

    /// The row's context, as the invariant runner needs it to run a check.
    pub fn row_context(&self, id: &EntryId) -> Option<Context> {
        self.fiber_of(id).and_then(|f| f.body().context())
    }

    /// Unload everything, LIFO, awaited.
    pub async fn shutdown(&self) {
        let mut uids: Vec<FiberUid> = self.rt.all().iter().map(|f| f.uid()).collect();
        uids.sort();
        for uid in uids.into_iter().rev() {
            self.rt.dispose(uid).await;
        }
        self.rows.lock().clear();
        self.tree.lock().clear();
        self.quiesce().await;
    }
}

/// Two committed views agree on every resolved binding identity.
fn same_targets(a: &ResolvedTargets, b: &ResolvedTargets) -> bool {
    let live = |v: &ResolvedTargets| -> BTreeMap<String, crate::service::ProviderUid> {
        v.iter()
            .filter_map(|(k, p)| p.map(|p| (k.clone(), p)))
            .collect()
    };
    live(a) == live(b)
}

// ---------------------------------------------------------------------------
// The plugin-backed factory: the one seam onto WP-2
// ---------------------------------------------------------------------------

/// One row's plugin, as the driver sees it.
///
/// It owns the row's `Context` and its parsed config; every lifecycle step here is one call into
/// `KernelCore`, in the order §0.3 mandates. The driver decides WHEN; this decides WHAT.
struct PluginBody {
    plugin: &'static str,
    entry: EntryId,
    inject: Inject,
    realms: BTreeMap<String, RealmLabel>,
    config: ErasedConfig,
    catalog: Arc<Catalog>,
    core: Arc<KernelCore>,
    root: Context,
    ctx: Mutex<Option<Context>>,
}

impl PluginBody {
    fn erased(&self) -> &dyn ErasedPlugin {
        self.catalog
            .get(self.plugin)
            .expect("the catalog was checked when the row was built")
    }
}

#[async_trait::async_trait]
impl FiberBody for PluginBody {
    fn inject(&self) -> Inject {
        self.inject.clone()
    }
    fn provides(&self) -> Vec<&'static str> {
        match *self.ctx.lock() {
            Some(ref c) => self.core.provided_by(c.fiber_uid()),
            None => Vec::new(),
        }
    }
    fn attach(&self, uid: FiberUid) {
        let ctx = self
            .root
            .for_row(uid, self.entry.clone(), self.plugin, self.inject.clone())
            .with_realms(self.realms.clone());
        *self.ctx.lock() = Some(ctx);
    }
    fn context(&self) -> Option<Context> {
        self.ctx.lock().clone()
    }
    async fn load(&self, _targets: Arc<crate::fiber::ResolvedTargets>) -> Result<(), PluginError> {
        let ctx = {
            let guard = self.ctx.lock();
            guard.clone().expect("attached before load")
        };
        // The COMMITTED view: captured before `apply`, immutable for this life (§0.3).
        let names: Vec<&str> = self
            .inject
            .required
            .iter()
            .chain(self.inject.optional.iter())
            .map(String::as_str)
            .collect();
        let view = CommittedView::capture(&self.core, &names, &self.realms, ctx.scope_key());
        let ctx = ctx.with_view(Arc::new(view));
        {
            *self.ctx.lock() = Some(ctx.clone());
        }
        let fut = self.erased().apply(ctx, self.config.clone());
        fut.await
    }
    async fn withdraw(&self) {
        let ctx = {
            let guard = self.ctx.lock();
            guard.clone()
        };
        if let Some(ctx) = ctx {
            self.core.withdraw_bindings_of(ctx.fiber_uid());
        }
    }
    async fn unwind(&self) {
        let ctx = {
            let guard = self.ctx.lock();
            guard.clone()
        };
        if let Some(ctx) = ctx {
            self.core.unwind_fiber(ctx.fiber_uid()).await;
        }
    }
}

struct PluginFactory {
    catalog: Arc<Catalog>,
    core: Arc<KernelCore>,
    root: Context,
}

impl PluginFactory {
    fn parse(&self, entry: &Entry) -> Result<(&'static str, ErasedConfig), ComposeError> {
        let name = entry.plugin.as_deref().unwrap_or_default();
        let p = self
            .catalog
            .get(name)
            .ok_or_else(|| ComposeError::UnknownPlugin {
                entry: entry.id.clone(),
                plugin: name.to_string(),
                layer: crate::config::LayerId::new("candidate"),
            })?;
        let cfg = p
            .parse(&entry.config)
            .map_err(|source| ComposeError::BadConfig {
                entry: entry.id.clone(),
                plugin: name.to_string(),
                layer: crate::config::LayerId::new("candidate"),
                source,
            })?;
        Ok((p.name(), cfg))
    }
}

impl BodyFactory for PluginFactory {
    fn build(&self, entry: &Entry) -> Result<Arc<dyn FiberBody>, ComposeError> {
        let (name, config) = self.parse(entry)?;
        // Entry ∪ plugin-static: the entry may ADD keys, never drop a static requirement (D1).
        let inject = entry
            .inject
            .union(&self.catalog.get(name).expect("just resolved").inject());
        Ok(Arc::new(PluginBody {
            plugin: name,
            entry: entry.id.clone(),
            inject,
            realms: entry.isolate.clone(),
            config,
            catalog: self.catalog.clone(),
            core: self.core.clone(),
            root: self.root.clone(),
            ctx: Mutex::new(None),
        }))
    }

    fn reconfigure(
        &self,
        current: &Arc<dyn FiberBody>,
        old: &Entry,
        new: &Entry,
    ) -> Result<(Arc<dyn FiberBody>, Reconfigure), ComposeError> {
        let (name, old_cfg) = self.parse(old)?;
        let (_, new_cfg) = self.parse(new)?;
        let ctx = current.context().unwrap_or_else(|| self.root.clone());
        let verdict = self
            .catalog
            .get(name)
            .expect("just resolved")
            .reconfigure(&ctx, &old_cfg, &new_cfg);
        let body = self.build(new)?;
        // The new fiber body inherits the running row's context, whichever the verdict: a
        // reconfigure never re-identifies the fiber (an ABSORBED config must not, and a RELOAD is
        // this same fiber going round the lifecycle again). Without the attach, the fiber's next
        // `load` finds no context at all.
        if let Some(c) = current.context() {
            body.attach(c.fiber_uid());
        }
        Ok((body, verdict))
    }

    fn static_name(&self, plugin: &str) -> Option<&'static str> {
        self.catalog.get(plugin).map(|p| p.name())
    }
}

struct CoreResolver {
    core: Arc<KernelCore>,
}

impl Resolver for CoreResolver {
    fn resolve(
        &self,
        key: &str,
        realm: Option<&RealmLabel>,
    ) -> Option<crate::service::ProviderUid> {
        let mut realms = BTreeMap::new();
        if let Some(r) = realm {
            realms.insert(key.to_string(), r.clone());
        }
        CommittedView::capture(&self.core, &[key], &realms, None).provider_of(key)
    }
}

/// Runs a plugin's check against the ROW's own context.
struct RowCheckHost {
    kernel: std::sync::Weak<Kernel>,
}

impl CheckHost for RowCheckHost {
    fn run(
        &self,
        entry: &EntryId,
        spec: &InvariantSpec,
    ) -> futures::future::BoxFuture<'static, Result<(), InvariantViolation>> {
        let ctx = self.kernel.upgrade().and_then(|k| k.row_context(entry));
        let check = spec.check;
        Box::pin(async move {
            match ctx {
                Some(ctx) => check(ctx).await,
                // A row that unloaded between collection and the run is not a violation.
                None => Ok(()),
            }
        })
    }
}
struct ContextEvents {
    ctx: Context,
}

impl KernelEvents for ContextEvents {
    fn fiber_state(&self, change: FiberStateChange) {
        self.ctx.emit::<crate::event::FiberStateChanged>(change);
    }
    fn config_update_failed(&self, error: Arc<ComposeError>) {
        self.ctx.emit::<crate::event::ConfigUpdateFailed>(error);
    }
    fn config_updated(&self, fingerprint: Fingerprint) {
        self.ctx.emit::<crate::event::ConfigUpdated>(fingerprint);
    }
    fn invariant_violated(&self, violation: Arc<InvariantViolation>) {
        self.ctx.emit::<crate::event::InvariantViolated>(violation);
    }
}

/// A snapshot of the whole tree, keyed by the composition fingerprint it reflects.
#[derive(Clone, Debug, serde::Serialize)]
pub struct TreeSnapshot {
    pub fingerprint: Fingerprint,
    pub rows: Vec<RowSnapshot>,
}

/// One row, as tests assert on it: structural facts, never a rendered string.
#[derive(Clone, Debug, serde::Serialize)]
pub struct RowSnapshot {
    pub id: EntryId,
    pub plugin: Option<String>,
    pub uid: Option<FiberUid>,
    pub state: FiberState,
    pub disabled: bool,
    pub unmet: Vec<String>,
    pub provides: Vec<&'static str>,
    pub realms: BTreeMap<String, RealmLabel>,
    pub children: Vec<RowSnapshot>,
}

/// An enabled row that is not ACTIVE. Fatal at boot, `kernel/rows-unresolved` at runtime
/// (Decision D12).
#[derive(Clone, Debug, serde::Serialize)]
pub struct UnresolvedRow {
    pub id: EntryId,
    pub plugin: Option<String>,
    pub state: FiberState,
    pub unmet: Vec<String>,
}

impl TreeSnapshot {
    /// Every enabled row that is not ACTIVE, depth-first.
    pub fn unresolved(&self) -> Vec<UnresolvedRow> {
        unresolved_rows(&self.rows)
    }
}

/// The free function [`TreeSnapshot::unresolved`] delegates to, so the launcher can run it over
/// rows it already has.
pub fn unresolved_rows(rows: &[RowSnapshot]) -> Vec<UnresolvedRow> {
    let mut out = Vec::new();
    fn walk(rows: &[RowSnapshot], out: &mut Vec<UnresolvedRow>) {
        for r in rows {
            if !r.disabled && r.state != FiberState::Active {
                out.push(UnresolvedRow {
                    id: r.id.clone(),
                    plugin: r.plugin.clone(),
                    state: r.state,
                    unmet: r.unmet.clone(),
                });
            }
            walk(&r.children, out);
        }
    }
    walk(rows, &mut out);
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::{Expr, Inject};
    use crate::fiber::tests::{TestStore, Trace};
    use std::collections::BTreeSet;

    // ---- a row builder, so the tests read like a bundle --------------------

    #[derive(Clone)]
    pub(crate) struct RowBuilder(pub Entry);

    pub(crate) fn row(id: &str) -> RowBuilder {
        RowBuilder(Entry {
            id: EntryId::new(id),
            plugin: None,
            config: serde_yaml::Value::Null,
            disabled: Expr::Literal(false),
            isolate: BTreeMap::new(),
            inject: Inject::none(),
            group: Vec::new(),
            include: None,
        })
    }

    impl RowBuilder {
        pub(crate) fn plugin(mut self, p: &str) -> Self {
            self.0.plugin = Some(p.to_string());
            self
        }
        pub(crate) fn cfg(mut self, k: &str, v: &str) -> Self {
            let map = match self.0.config {
                serde_yaml::Value::Mapping(m) => m,
                _ => serde_yaml::Mapping::new(),
            };
            let mut map = map;
            map.insert(k.into(), v.into());
            self.0.config = serde_yaml::Value::Mapping(map);
            self
        }
        pub(crate) fn disabled(mut self, d: bool) -> Self {
            self.0.disabled = Expr::Literal(d);
            self
        }
        pub(crate) fn inject(mut self, keys: &[&str]) -> Self {
            self.0
                .inject
                .required
                .extend(keys.iter().map(|s| s.to_string()));
            self
        }
        pub(crate) fn inject_optional(mut self, keys: &[&str]) -> Self {
            self.0
                .inject
                .optional
                .extend(keys.iter().map(|s| s.to_string()));
            self
        }
        pub(crate) fn isolate(mut self, key: &str, realm: &str) -> Self {
            self.0
                .isolate
                .insert(key.to_string(), RealmLabel::new(realm));
            self
        }
        pub(crate) fn child(mut self, c: RowBuilder) -> Self {
            self.0.group.push(c.0);
            self
        }
    }

    impl From<RowBuilder> for Entry {
        fn from(b: RowBuilder) -> Entry {
            b.0
        }
    }

    // ---- a recording factory, standing in for a plugin crate ---------------

    /// A body named `<row>/<plugin>`, so a trace line names the row AND the plugin that ran.
    struct TestBody {
        name: String,
        trace: Trace,
        store: Arc<TestStore>,
        uid: Mutex<Option<FiberUid>>,
        inject: Inject,
        realms: BTreeMap<String, RealmLabel>,
        provides: Option<&'static str>,
        entry: EntryId,
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
        async fn load(&self, _view: Arc<ResolvedTargets>) -> Result<(), PluginError> {
            if let Some(key) = self.provides {
                let uid = self.uid.lock().expect("attached");
                self.store.provide(key, self.realms.get(key), uid);
            }
            self.trace.push(format!("{}:apply", self.name));
            let _ = &self.entry;
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
            self.trace.push(format!("{}:unwind", self.name));
        }
    }

    struct TestFactory {
        trace: Trace,
        store: Arc<TestStore>,
    }

    impl TestFactory {
        fn body(&self, entry: &Entry) -> Arc<dyn FiberBody> {
            let plugin = entry.plugin.clone().unwrap_or_else(|| "group".to_string());
            Arc::new(TestBody {
                name: format!("{}/{}", entry.id.as_str(), plugin),
                trace: self.trace.clone(),
                store: self.store.clone(),
                uid: Mutex::new(None),
                inject: entry.inject.clone(),
                realms: entry.isolate.clone(),
                provides: if plugin == "provider" {
                    Some("greeting")
                } else {
                    None
                },
                entry: entry.id.clone(),
            })
        }
    }

    fn cfg_field(e: &Entry, k: &str) -> Option<String> {
        e.config
            .get(k)
            .and_then(|v| v.as_str().map(|s| s.to_string()))
    }

    impl BodyFactory for TestFactory {
        fn build(&self, entry: &Entry) -> Result<Arc<dyn FiberBody>, ComposeError> {
            match entry.plugin.as_deref() {
                Some("nope") => Err(ComposeError::UnknownPlugin {
                    entry: entry.id.clone(),
                    plugin: "nope".to_string(),
                    layer: crate::config::LayerId::new("candidate"),
                }),
                _ => Ok(self.body(entry)),
            }
        }
        fn reconfigure(
            &self,
            _current: &Arc<dyn FiberBody>,
            old: &Entry,
            new: &Entry,
        ) -> Result<(Arc<dyn FiberBody>, Reconfigure), ComposeError> {
            let plugin = new.plugin.clone().unwrap_or_else(|| "group".to_string());
            self.trace
                .push(format!("{}/{}:reconfigure", new.id.as_str(), plugin));
            // `log_level` is immaterial; everything else reloads. This mirrors the `hello`
            // fixture's rule and is what proves the plugin, not the kernel, decides.
            let material = cfg_field(old, "who") != cfg_field(new, "who");
            let verdict = if material {
                Reconfigure::Reload
            } else {
                Reconfigure::Applied
            };
            Ok((self.build(new)?, verdict))
        }
        fn static_name(&self, plugin: &str) -> Option<&'static str> {
            match plugin {
                "one" => Some("one"),
                "two" => Some("two"),
                "provider" => Some("provider"),
                _ => None,
            }
        }
    }

    #[derive(Default)]
    pub(crate) struct RecordingEvents {
        pub failures: Mutex<Vec<String>>,
        pub updated: Mutex<Vec<Fingerprint>>,
    }

    impl KernelEvents for RecordingEvents {
        fn config_update_failed(&self, error: Arc<ComposeError>) {
            self.failures.lock().push(error.to_string());
        }
        fn config_updated(&self, fingerprint: Fingerprint) {
            self.updated.lock().push(fingerprint);
        }
    }

    pub(crate) struct TreeHarness {
        pub kernel: Arc<Kernel>,
        pub trace: Trace,
        pub events: Arc<RecordingEvents>,
    }

    impl TreeHarness {
        pub(crate) fn new() -> TreeHarness {
            let trace = Trace::default();
            let store = Arc::new(TestStore::default());
            let events = Arc::new(RecordingEvents::default());
            let factory = Arc::new(TestFactory {
                trace: trace.clone(),
                store: store.clone(),
            });
            let kernel = Kernel::with_parts(
                None,
                KernelOptions::default(),
                factory,
                store.clone(),
                events.clone(),
            );
            TreeHarness {
                kernel,
                trace,
                events,
            }
        }

        pub(crate) async fn apply(&self, rows: Vec<RowBuilder>) {
            self.try_apply(rows).await.expect("tree applied");
        }

        pub(crate) async fn try_apply(&self, rows: Vec<RowBuilder>) -> Result<(), KernelError> {
            let tree: Vec<Entry> = rows.into_iter().map(Entry::from).collect();
            self.kernel.update_tree(tree).await
        }

        pub(crate) fn fiber(&self, id: &str) -> Option<FiberHandle> {
            self.kernel.fiber(&EntryId::new(id))
        }
        pub(crate) fn state(&self, id: &str) -> FiberState {
            self.fiber(id)
                .map(|f| f.state())
                .unwrap_or(FiberState::Inactive)
        }
        pub(crate) fn uid(&self, id: &str) -> FiberUid {
            self.fiber(id).expect("fiber exists").uid()
        }
        pub(crate) fn realm(&self, id: &str, key: &str) -> Option<String> {
            self.kernel
                .rows
                .lock()
                .get(&EntryId::new(id))
                .and_then(|r| r.entry.isolate.get(key).map(|r| r.as_str().to_string()))
        }
    }

    fn keys(v: &[&str]) -> BTreeSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[tokio::test]
    async fn shutdown_unloads_everything_lifo() {
        let h = TreeHarness::new();
        h.apply(vec![
            row("a").plugin("one"),
            row("b").plugin("one"),
            row("c").plugin("one"),
        ])
        .await;
        h.trace.push("--");
        h.kernel.shutdown().await;

        let t = h.trace.entries();
        let base = t.iter().position(|e| e == "--").unwrap();
        let unwinds: Vec<&String> = t[base..]
            .iter()
            .filter(|e| e.ends_with(":unwind"))
            .collect();
        assert_eq!(
            unwinds,
            vec!["c/one:unwind", "b/one:unwind", "a/one:unwind"],
            "shutdown unwinds LIFO: {t:?}"
        );
    }

    #[tokio::test]
    async fn snapshot_reports_unmet_keys() {
        let h = TreeHarness::new();
        h.apply(vec![row("c").plugin("one").inject(&["greeting"])])
            .await;
        let rows = h.kernel.rows_snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].state, FiberState::Pending);
        assert_eq!(keys(&["greeting"]), rows[0].unmet.iter().cloned().collect());
        let unresolved = unresolved_rows(&rows);
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].id.as_str(), "c");
        assert_eq!(unresolved[0].unmet, vec!["greeting".to_string()]);
    }

    #[tokio::test]
    async fn update_failure_keeps_the_last_good_tree() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one")]).await;
        let uid = h.uid("a");

        let err = h
            .try_apply(vec![row("a").plugin("one"), row("bad").plugin("nope")])
            .await
            .expect_err("an unknown plugin rejects the whole candidate");
        assert!(err.to_string().contains("nope"), "{err}");

        assert_eq!(h.state("a"), FiberState::Active);
        assert_eq!(h.uid("a"), uid, "the last good tree is untouched");
        assert!(h.fiber("bad").is_none());
    }

    #[tokio::test]
    async fn update_failure_emits_config_update_failed() {
        let h = TreeHarness::new();
        h.apply(vec![row("a").plugin("one")]).await;
        let _ = h.try_apply(vec![row("bad").plugin("nope")]).await;
        let failures = h.events.failures.lock().clone();
        assert_eq!(
            failures.len(),
            1,
            "exactly one broadcast per rejected candidate"
        );
        assert!(failures[0].contains("not in the catalog"), "{:?}", failures);
    }
}

/// The catalog-backed path, end to end: real `Plugin` impls, a real `Catalog`, the real
/// `PluginFactory`, the real binding store. The tests above drive the lifecycle through a
/// recording factory because ORDER is what they assert; this one proves the wiring underneath is
/// not a stub.
#[cfg(test)]
mod e2e {
    use super::*;
    use crate::catalog::PluginRegistration;
    use crate::plugin::{Plugin, Shim};
    use crate::service::ServiceKey;
    use std::sync::OnceLock;

    struct Greet;
    impl ServiceKey for Greet {
        type Value = String;
        const NAME: &'static str = "e2e-greeting";
    }

    fn seen() -> &'static Mutex<Vec<String>> {
        static SEEN: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
        SEEN.get_or_init(|| Mutex::new(Vec::new()))
    }

    #[derive(
        serde::Serialize, serde::Deserialize, schemars::JsonSchema, PartialEq, Debug, Default,
    )]
    struct EchoConfig {
        #[serde(default)]
        suffix: String,
    }

    struct EchoProvider;
    #[async_trait::async_trait]
    impl Plugin for EchoProvider {
        const NAME: &'static str = "e2e-echo";
        type Config = EchoConfig;
        async fn apply(ctx: Context, cfg: Arc<EchoConfig>) -> Result<(), PluginError> {
            ctx.provide::<Greet>(format!("hello{}", cfg.suffix))
                .await
                .map_err(|e| PluginError::new(ctx.entry_id().clone(), anyhow::anyhow!("{e}")))?;
            Ok(())
        }
    }

    #[derive(
        serde::Serialize, serde::Deserialize, schemars::JsonSchema, PartialEq, Debug, Default,
    )]
    struct ConsumerConfig {
        #[serde(default)]
        who: String,
    }

    struct Consumer;
    #[async_trait::async_trait]
    impl Plugin for Consumer {
        const NAME: &'static str = "e2e-consumer";
        type Config = ConsumerConfig;
        fn inject() -> Inject {
            Inject::required(["e2e-greeting"])
        }
        async fn apply(ctx: Context, cfg: Arc<ConsumerConfig>) -> Result<(), PluginError> {
            let greeting = ctx
                .get::<Greet>()
                .map_err(|e| PluginError::new(ctx.entry_id().clone(), anyhow::anyhow!("{e}")))?;
            seen().lock().push(format!("{greeting} {}", cfg.who));
            Ok(())
        }
    }

    fn catalog() -> Catalog {
        Catalog::from_parts(vec![
            PluginRegistration {
                name: "e2e-echo",
                ctor: || Box::new(Shim::<EchoProvider>::new()),
            },
            PluginRegistration {
                name: "e2e-consumer",
                ctor: || Box::new(Shim::<Consumer>::new()),
            },
        ])
        .unwrap()
    }

    #[tokio::test]
    async fn a_catalog_backed_tree_activates_and_unloads() {
        seen().lock().clear();
        let kernel = Kernel::new(catalog(), KernelOptions::default());
        let tree: Vec<Entry> = vec![
            tests::row("p").plugin("e2e-echo").into(),
            tests::row("c")
                .plugin("e2e-consumer")
                .cfg("who", "world")
                .into(),
        ];
        kernel.update_tree(tree.clone()).await.unwrap();

        let rows = kernel.rows_snapshot();
        assert_eq!(rows[0].state, FiberState::Active, "{rows:?}");
        assert_eq!(rows[1].state, FiberState::Active, "{rows:?}");
        assert_eq!(seen().lock().clone(), vec!["hello world".to_string()]);
        assert_eq!(rows[0].provides, vec!["e2e-greeting"]);
        assert_eq!(kernel.core().binding_count(), 1);

        // Disabling the provider withdraws the binding and leaves the consumer PENDING on it.
        let disabled: Vec<Entry> = vec![
            tests::row("p").plugin("e2e-echo").disabled(true).into(),
            tests::row("c")
                .plugin("e2e-consumer")
                .cfg("who", "world")
                .into(),
        ];
        kernel.update_tree(disabled).await.unwrap();
        let rows = kernel.rows_snapshot();
        assert_eq!(rows[0].state, FiberState::Inactive);
        assert_eq!(rows[1].state, FiberState::Pending, "{rows:?}");
        assert_eq!(rows[1].unmet, vec!["e2e-greeting".to_string()]);
        assert_eq!(
            kernel.core().binding_count(),
            0,
            "the provision was an effect, and it unwound"
        );

        kernel.shutdown().await;
        assert_eq!(kernel.core().binding_count(), 0);
    }
}
