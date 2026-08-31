# Phase 0 — the center: design and work breakdown

Authority: `REQUIREMENTS.md` §0 (0.1–0.5), §13, §17 Phase 0. Where this document and REQUIREMENTS
disagree, REQUIREMENTS wins and this document is the bug. Everything below that REQUIREMENTS does
not settle is listed in §6 as a labelled decision.

Reference implementations read as algorithm sources only (§13): Cordis 4 / the paper,
`dshbox/cordis-rs` (string-keyed, sync reconcile), `cordis-core` (typed keys, tokio). **Phase 0
depends on neither.** Revisit at Phase 4 (§15 item 5).

---

## 1. Crates

One cargo workspace at the repo root. `plugins/*` is a glob member. Package names are exact.

| path | package | depends on | provides (ctx keys) | injects |
|---|---|---|---|---|
| `crates/bough-util` | `bough-util` | — | none (a library, no ctx key — §0.1 item 3) | none |
| `crates/bough-kernel` | `bough-kernel` | `bough-util` | none (domain-blind — §0.1 item 1) | none |
| `crates/bough-llm` | `bough-llm` | — | none in Phase 0 (wrapped by a plugin at Phase 2) | none |
| `crates/bough` | `bough` (bin `bough`) | `bough-kernel`, `bough-util`, every `bough-plugin-*` | none (composition only — §0.1 item 2) | none |
| `plugins/hello` | `bough-plugin-hello` | `bough-kernel` | `greeting` (from rows `greeting-echo`, `greeting-shout`) | `greeting` (row `hello`) |

Phase 0 defines **no service keys in the kernel and none in the launcher**. The only key that
exists is the fixture's `greeting`, and it exists to prove the kernel, not to serve a product need.

`crates/bough` depends on every plugin crate for one reason only: linking them so their
`inventory::submit!` registrations land in the binary. It never names a plugin type.

New workspace dependencies Phase 0 adds to the root `Cargo.toml`:
`parking_lot` (kernel state lock), `include_dir` (embedded `profiles/` + `bundles/`),
`futures` / `async-trait` (already present), `pin-project-lite` (effect halt futures).
Everything else Phase 0 needs — `serde_yaml`, `schemars`, `inventory`, `sha2`, `notify`,
`notify-debouncer-full`, `clap`, `thiserror`, `tokio`, `tracing` — is already pinned there.

### 1.1 The reqwest 0.12 / 0.13 stance (recorded, per §17 Phase 0)

Recorded as of this branch, not as an aspiration:

- `Cargo.lock` on `rebuild` today carries **reqwest 0.12.28 only**. `rmcp` is not a dependency yet;
  the tree was cleared down to `bough-llm` in c77fffb1.
- `bough-llm` uses reqwest 0.12 directly, for exactly one thing: the hand-rolled Anthropic SSE
  transport (`crates/bough-llm/src/sse.rs`, one shared `reqwest::Client`). The workspace pin is
  `reqwest = { version = "0.12", features = ["json", "stream", "blocking"] }`.
- At Phase 6, `rmcp` arrives and pulls reqwest 0.13 transitively, bridged through
  `OAuthHttpClient`. Cargo will then resolve **both majors into the same binary**. That is the
  arrangement REQUIREMENTS §13 blesses, and it **STANDS**: bough-llm is not migrated to 0.13 to
  chase a single version, and rmcp is not vendored to hold it at 0.12.
- **Phase 0 changes nothing and adds no reqwest dependency of its own.** The kernel, the launcher
  and `bough-util` must not depend on reqwest at all; the dual version stays confined to
  `bough-llm` (0.12) and, later, `rmcp` (0.13). A Phase-0 review that finds `reqwest` in
  `bough-kernel`'s dependency list is a failed review.
- Consequence to expect and not to "fix": `cargo tree -d` will report a duplicate reqwest from
  Phase 6 onward, and `cargo audit` may report advisories against whichever major lags. Neither is
  a Phase 0 action.

### 1.2 CI

The layout does not need CI changes: `cargo build/test/clippy --workspace --all-targets` already
covers `plugins/*` through the glob member. One line is wrong for a different reason — CI's
`on.push.branches` is `[main]` and the rebuild happens on `rebuild`. WP-1 adds `rebuild` to that
list. Nothing else in `.github/workflows/ci.yml` is touched in Phase 0 (Decision D17).

---

## 2. Public API

This section is the contract between the work packages. Signatures are normative: an implementer
may add private items freely, but may not change a signature here without editing this document
first. Everything is `Send + Sync + 'static` (Decision D13); the kernel runs on one tokio runtime.

### 2.1 `bough-util`

```rust
// crates/bough-util/src/id.rs — branded ids (§0.2 "opaque cross-boundary ids are branded types")
/// Declares a newtype over `Arc<str>` with Debug/Display/Clone/Eq/Hash/Ord/Serialize/Deserialize/
/// FromStr and `fn as_str(&self) -> &str`. No `From<String>`: construction is `Name::new(s)`, so a
/// bare string never becomes an id by inference.
#[macro_export] macro_rules! brand_id { ($(#[$m:meta])* $vis:vis struct $name:ident;) => { ... } }

// crates/bough-util/src/home.rs
pub fn home_dir() -> PathBuf;                       // $HOME, or the platform home
pub fn bough_home() -> PathBuf;                     // $BOUGH_HOME, else ~/.bough
pub fn bough_path(rel: impl AsRef<Path>) -> PathBuf;// bough_home().join(rel), normalised
pub fn user_patch_path() -> PathBuf;                // bough_path("bough.patch.yml")
pub fn ensure_dir(p: &Path) -> std::io::Result<()>;

// crates/bough-util/src/time.rs
#[derive(Debug, thiserror::Error)] #[error("timed out after {0:?}")] pub struct TimedOut(pub Duration);
pub async fn with_timeout<T>(d: Duration, f: impl Future<Output = T>) -> Result<T, TimedOut>;
#[derive(Clone, Copy, Debug)] pub struct Deadline(Instant);
impl Deadline { pub fn in_(d: Duration) -> Self; pub fn remaining(&self) -> Option<Duration>; pub fn expired(&self) -> bool; }
```

`bough-util` is pure: no tokio runtime creation, no global state, no logging setup. `home_dir` and
`bough_home` read the environment on every call so tests can set `BOUGH_HOME` per test.

### 2.2 Kernel: service keys and the store

```rust
// crates/bough-kernel/src/service.rs
/// A capability slot. `NAME` is the string that appears in `inject:` lists, in `isolate:` maps,
/// in `--dump-config`, and in error messages. `Value` is Sized (Decision D5): a trait-object
/// service is exposed as a concrete handle newtype owned by the Service Definition, e.g.
/// `pub struct LedgerHandle(Arc<dyn Ledger>);`.
pub trait ServiceKey: Send + Sync + 'static {
    type Value: Send + Sync + 'static;
    const NAME: &'static str;
}

/// Identity of one binding. Dependents target this, never the value (§0.3).
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize)]
pub struct ProviderUid { pub fiber: FiberUid, pub seq: u64 }

/// Returned by `Context::provide`. Dropping it does NOT withdraw; the owning fiber's
/// accumulator does, LIFO, on unload.
pub struct ServiceSlot<K: ServiceKey> { /* .. */ }
impl<K: ServiceKey> ServiceSlot<K> {
    pub fn uid(&self) -> ProviderUid;
    pub fn effect(&self) -> &EffectHandle;
    /// Overwrite the value in place. Same `ProviderUid`; dependents are NOT notified (§0.3).
    pub fn set(&self, value: K::Value);
    /// Withdraw and re-provide: a new `seq`, so dependents recompute and reload (§0.3).
    pub async fn republish(&self, value: K::Value);
    /// Withdraw now. Idempotent.
    pub async fn withdraw(&self);
}
```

### 2.3 Kernel: `Context`

`Context` is a cheap clone (`Arc` inside) carrying: the owning `FiberUid`, the realm map from
`isolate:`, the interception map, and the scope chain. Cloning never changes ownership: an effect
registered through any clone belongs to the same fiber.

```rust
// crates/bough-kernel/src/context.rs
#[derive(Clone)]
pub struct Context { /* .. */ }

impl Context {
    // ---- identity -------------------------------------------------------------
    pub fn fiber(&self) -> FiberHandle;
    pub fn entry_id(&self) -> &EntryId;
    pub fn plugin_name(&self) -> &'static str;
    pub fn kernel(&self) -> &Kernel;

    // ---- services -------------------------------------------------------------
    /// Provide `K` in this context's realm for `K::NAME`. Registered as an effect of the owning
    /// fiber; withdrawn on unload BEFORE any other inverse of that fiber runs (§0.3).
    pub async fn provide<K: ServiceKey>(&self, value: K::Value) -> Result<ServiceSlot<K>, KernelError>;

    /// Read `K` from this fiber's COMMITTED view (bindings captured at activation, §0.3), so a
    /// plugin sees the same providers for its whole life, teardown included.
    /// Err(UndeclaredService) if `K::NAME` is in neither the fiber's effective inject set nor its
    /// own provisions — the capability check of §0.3, at the point of use.
    /// Err(ServiceUnavailable) if declared optional and absent.
    pub fn get<K: ServiceKey>(&self) -> Result<Arc<K::Value>, KernelError>;
    pub fn try_get<K: ServiceKey>(&self) -> Result<Option<Arc<K::Value>>, KernelError>;
    /// The live store, bypassing the committed view. Only the kernel's own diagnostics and the
    /// launcher use this; a plugin calling it is a review failure.
    pub fn peek_live<K: ServiceKey>(&self) -> Option<Arc<K::Value>>;

    // ---- effects --------------------------------------------------------------
    /// Runs `body` to completion inline, then returns. Inverses deferred inside `body` are
    /// prepended to the fiber's accumulator (LIFO recovery, §0.3). The 95% case: registering a
    /// service, a listener, a pane, a child entry.
    pub async fn effect<F, Fut>(&self, body: F) -> Result<EffectHandle, PluginError>
    where F: FnOnce(EffectCtx) -> Fut + Send + 'static,
          Fut: Future<Output = Result<(), PluginError>> + Send + 'static;

    /// Spawns `body` and returns immediately. Disposal halts it at its next
    /// `EffectCtx::checkpoint().await`, then unwinds whatever it deferred, LIFO.
    pub fn effect_spawn<F, Fut>(&self, body: F) -> EffectHandle
    where F: FnOnce(EffectCtx) -> Fut + Send + 'static,
          Fut: Future<Output = Result<(), PluginError>> + Send + 'static;

    // ---- nested mounts (children are effects of the parent, §0.3) --------------
    /// Mounts `entry` as a child of this fiber. Unloading the parent cascades.
    pub async fn mount(&self, entry: Entry) -> Result<FiberHandle, KernelError>;

    // ---- events ---------------------------------------------------------------
    pub async fn on<E: EmitEvent, F, Fut>(&self, f: F) -> Result<EffectHandle, PluginError>
        where F: Fn(E::Payload) -> Fut + Send + Sync + 'static, Fut: Future<Output = ()> + Send + 'static;
    pub async fn on_parallel<E: ParallelEvent, F, Fut>(&self, f: F) -> Result<EffectHandle, PluginError>
        where F: Fn(E::Payload) -> Fut + Send + Sync + 'static, Fut: Future<Output = ()> + Send + 'static;
    pub async fn on_serial<E: SerialEvent, F, Fut>(&self, f: F) -> Result<EffectHandle, PluginError>
        where F: Fn(E::Payload) -> Fut + Send + Sync + 'static, Fut: Future<Output = Option<E::Output>> + Send + 'static;
    pub async fn on_waterfall<E: WaterfallEvent, F, Fut>(&self, f: F) -> Result<EffectHandle, PluginError>
        where F: Fn(E::Value, Next<E>) -> Fut + Send + Sync + 'static, Fut: Future<Output = E::Value> + Send + 'static;
    /// `_with` variants take `ListenerOpts { prepend, scope }`; the four above are
    /// `on_*_with(ListenerOpts::default(), f)`.
    pub async fn on_with<E: EmitEvent, F, Fut>(&self, opts: ListenerOpts, f: F) -> Result<EffectHandle, PluginError> where /* as above */;
    // ... on_parallel_with, on_serial_with, on_waterfall_with, same shape.

    pub fn emit<E: EmitEvent>(&self, payload: E::Payload);
    pub async fn parallel<E: ParallelEvent>(&self, payload: E::Payload);
    pub async fn serial<E: SerialEvent>(&self, payload: E::Payload) -> Option<E::Output>;
    pub async fn waterfall<E: WaterfallEvent>(&self, value: E::Value) -> E::Value;

    // ---- isolate / intercept (§0.3) -------------------------------------------
    /// A child context resolving `K` in `realm`. Entries sharing a realm label share the binding.
    pub fn isolate<K: ServiceKey>(&self, realm: RealmLabel) -> Context;
    /// Per-context metadata a provider consults on use. Does NOT affect satisfaction and does NOT
    /// reload anyone; changeable at runtime.
    pub fn intercept<K: ServiceKey>(&self, metadata: serde_yaml::Value) -> Context;
    pub fn interception<K: ServiceKey>(&self) -> Option<Arc<serde_yaml::Value>>;
    pub fn set_interception<K: ServiceKey>(&self, metadata: serde_yaml::Value);
}
```

### 2.4 Kernel: effects

```rust
// crates/bough-kernel/src/effect.rs
pub struct EffectCtx { /* .. */ }
impl EffectCtx {
    pub fn ctx(&self) -> &Context;
    /// Push an inverse. Inverses run LIFO within the effect; effects run LIFO within the fiber.
    pub fn defer<F, Fut>(&self, inverse: F) where F: FnOnce() -> Fut + Send + 'static, Fut: Future<Output = ()> + Send + 'static;
    pub fn defer_sync(&self, inverse: impl FnOnce() + Send + 'static);
    /// The halt boundary. `Err(Halted)` once disposal has begun; the body must return promptly.
    pub async fn checkpoint(&self) -> Result<(), Halted>;
    pub fn is_halted(&self) -> bool;
}

#[derive(Debug, Clone, Copy, thiserror::Error)] #[error("effect halted by disposal")] pub struct Halted;

#[derive(Clone)]
pub struct EffectHandle { /* .. */ }
impl EffectHandle {
    /// Halts an in-flight body at its next checkpoint, then unwinds its inverses LIFO.
    /// Fires AT MOST ONCE, whichever clone calls it, however many times: an `AtomicBool` claims
    /// the run and later callers await the same completion.
    pub async fn dispose(&self);
    pub fn dispose_detached(&self);
    pub fn is_disposed(&self) -> bool;
}
```

### 2.5 Kernel: events

Four traits, not one trait plus a runtime mode enum (Decision D3): the dispatch mode of an event is
part of its public contract (§0.2) and is therefore checked by the compiler. `MODE` exists for the
`--dump-config`/catalog surface and for the §15 item 7 `cargo xtask` gate later.

```rust
// crates/bough-kernel/src/event.rs
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
pub enum DispatchMode { Emit, Parallel, Serial, Waterfall }

/// Fire and forget. `emit` returns immediately; listeners run in registration order on the
/// kernel's dispatch task. No return value.
pub trait EmitEvent: Send + Sync + 'static {
    const NAME: &'static str; const MODE: DispatchMode = DispatchMode::Emit;
    type Payload: Clone + Send + Sync + 'static;
}
/// Awaited fan-out: all listeners are started concurrently and `parallel` returns when all have
/// finished. No return value.
pub trait ParallelEvent: Send + Sync + 'static {
    const NAME: &'static str; const MODE: DispatchMode = DispatchMode::Parallel;
    type Payload: Clone + Send + Sync + 'static;
}
/// Awaited in registration order. The FIRST listener returning `Some` wins; the rest do not run.
pub trait SerialEvent: Send + Sync + 'static {
    const NAME: &'static str; const MODE: DispatchMode = DispatchMode::Serial;
    type Payload: Clone + Send + Sync + 'static;
    type Output: Send + 'static;
}
/// Around-middleware. A listener receives the value and `next` and MUST call `next` to delegate;
/// returning without calling it short-circuits the rest of the chain (§0.3).
pub trait WaterfallEvent: Send + Sync + 'static {
    const NAME: &'static str; const MODE: DispatchMode = DispatchMode::Waterfall;
    type Value: Send + 'static;
}

pub struct Next<E: WaterfallEvent> { /* .. */ }
impl<E: WaterfallEvent> Next<E> {
    /// Runs the remainder of the chain. Consumes `self`, so "call next at most once" is a type error.
    pub async fn run(self, value: E::Value) -> E::Value;
}

#[derive(Clone, Default, Debug)]
pub struct ListenerOpts { pub prepend: bool, pub scope: Option<ScopeKey> }
```

**Containment (§0.3, Decision D4).** Every listener invocation is wrapped: a panic is caught, a
returned `Err` is logged, and in both cases the dispatch continues. Per mode: `emit`/`parallel`
skip the listener; `serial` treats it as `None` and moves to the next listener; `waterfall` treats
it as *delegate unchanged* and continues the chain with the value it was given. Every containment
emits `kernel/listener-failed`.

**Kernel-owned events.** These are the whole Phase 0 event catalog; they carry no domain
vocabulary.

| event | type | mode | payload | meaning |
|---|---|---|---|---|
| `config-update-failed` | `ConfigUpdateFailed` | Emit | `Arc<ComposeError>` | a candidate tree was rejected; the last good tree is still running (§0.3) |
| `config-updated` | `ConfigUpdated` | Emit | `Fingerprint` | a candidate tree was accepted and reconciled |
| `kernel/fiber-state` | `FiberStateChanged` | Emit | `FiberStateChange { uid, id, from, to, error }` | one lifecycle transition |
| `kernel/rows-unresolved` | `RowsUnresolved` | Emit | `Arc<Vec<UnresolvedRow>>` | after quiescence, enabled rows that are not ACTIVE |
| `kernel/listener-failed` | `ListenerFailed` | Emit | `ListenerFailure { event, entry, detail }` | a contained listener panic/error |
| `kernel/invariant-violated` | `InvariantViolated` | Emit | `Arc<InvariantViolation>` | the invariant runner found a violation |

`config-update-failed` is spelled without a `kernel/` prefix because §0.3 names it verbatim.

### 2.6 Kernel: scope

```rust
// crates/bough-kernel/src/scope.rs
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ScopeKey { id: Arc<str>, parent: Option<Arc<ScopeKey>> }
impl ScopeKey {
    pub fn new(id: impl Into<Arc<str>>) -> Self;
    pub fn child(&self, id: impl Into<Arc<str>>) -> Self;
    pub fn ancestors(&self) -> impl Iterator<Item = &ScopeKey>;   // self first, then up
}

/// A tagged context whose registrations are scope-VISIBLE and scope-LIFETIME: services provided
/// through it bind only for `key` and its descendants; listeners registered through it are
/// admitted only for dispatches targeted at `key` or a descendant of `key`; disposing the scope
/// unwinds all of them.
pub fn create_scope(ctx: &Context, key: ScopeKey) -> ScopeGuard;
pub struct ScopeGuard { /* .. */ }
impl ScopeGuard { pub fn context(&self) -> &Context; pub fn key(&self) -> &ScopeKey; pub fn effect(&self) -> &EffectHandle; }

/// Routes a dispatch to untagged listeners PLUS the subject's own PLUS its ancestors'
/// (admission extends UP, §0.3).
pub fn scope_target<'a>(base: &'a Context, key: &ScopeKey) -> ScopedDispatch<'a>;
pub struct ScopedDispatch<'a> { /* .. */ }
impl ScopedDispatch<'_> {
    pub fn emit<E: EmitEvent>(&self, payload: E::Payload);
    pub async fn parallel<E: ParallelEvent>(&self, payload: E::Payload);
    pub async fn serial<E: SerialEvent>(&self, payload: E::Payload) -> Option<E::Output>;
    pub async fn waterfall<E: WaterfallEvent>(&self, value: E::Value) -> E::Value;
}
```

Two directions, deliberately opposite (§0.3):

- **Views inherit DOWN.** Resolving `K` from a context tagged `a/b/c`: try scope `a/b/c`, then
  `a/b`, then `a`, then the untagged global binding. Nearest shadows farthest.
- **Admission extends UP.** `scope_target(ctx, a/b/c)` delivers to listeners tagged `a/b/c`,
  `a/b`, `a`, and untagged listeners. It does NOT deliver to a listener tagged `a/b/c/d`, nor to a
  sibling `a/b/x`.

Scopes route trusted in-process plugins. They are not sandboxes and not authority boundaries.

### 2.7 Kernel: plugins, catalog, fibers

```rust
// crates/bough-kernel/src/plugin.rs
/// A plugin is a set of associated functions, not an object: the fiber owns the config and the
/// context, so the shim is a ZST and the catalog constructor is trivial.
#[async_trait::async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Catalog name; matches an entry's `plugin:` field.
    const NAME: &'static str;
    type Config: serde::de::DeserializeOwned + serde::Serialize + schemars::JsonSchema
               + PartialEq + std::fmt::Debug + Send + Sync + 'static;

    /// Static injection declaration. Unioned with the entry's `inject:` field (Decision D1).
    fn inject() -> Inject { Inject::none() }

    /// PURE, SYNCHRONOUS validation (§0.5). No I/O, no clock, no network. A check that needs I/O
    /// belongs in `apply`.
    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> { Ok(()) }

    /// Register the plugin's effects. Returning Ok means ACTIVE. Everything registered here is
    /// an effect of this fiber and is unwound on unload.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError>;

    /// A new config is always HANDED to the plugin; the plugin decides whether it is material
    /// (§0.3). Default: material iff `old != new` (Decision D7).
    fn reconfigure(_ctx: &Context, old: &Self::Config, new: &Self::Config) -> Reconfigure {
        if old == new { Reconfigure::Applied } else { Reconfigure::Reload }
    }

    /// §0.2: every plugin crate owns an invariant module, or states why it has none.
    fn invariants() -> Vec<InvariantSpec> { Vec::new() }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reconfigure { /// absorbed live, no reload
                        Applied,
                       /// unload then load with the new config
                        Reload }

/// Object-safe boundary. `impl<P: Plugin> ErasedPlugin for Shim<P>` is blanket, written once.
pub trait ErasedPlugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn inject(&self) -> Inject;
    fn schema(&self) -> schemars::Schema;
    /// deserialize + `Plugin::validate`; the returned handle carries both the typed value and the
    /// canonicalised yaml used by `--dump-config` and the fingerprint.
    fn parse(&self, raw: &serde_yaml::Value) -> Result<ErasedConfig, ConfigError>;
    fn apply(&self, ctx: Context, cfg: ErasedConfig) -> futures::future::BoxFuture<'static, Result<(), PluginError>>;
    fn reconfigure(&self, ctx: &Context, old: &ErasedConfig, new: &ErasedConfig) -> Reconfigure;
    fn invariants(&self) -> Vec<InvariantSpec>;
}
#[derive(Clone)] pub struct ErasedConfig { typed: Arc<dyn Any + Send + Sync>, yaml: Arc<serde_yaml::Value> }

// crates/bough-kernel/src/catalog.rs
pub struct PluginRegistration { pub name: &'static str, pub ctor: fn() -> Box<dyn ErasedPlugin> }
inventory::collect!(PluginRegistration);

/// `register_plugin!(HelloPlugin);` at the bottom of a plugin crate's lib.rs. That single line is
/// what "the catalog is compile-time" (§0.4) means in practice.
#[macro_export] macro_rules! register_plugin { ($t:ty) => { ... } }

pub struct Catalog { /* name -> Box<dyn ErasedPlugin> */ }
impl Catalog {
    /// Err on a duplicate name: two crates claiming one catalog name is a build-time bug that
    /// must not become a silent last-wins.
    pub fn from_inventory() -> Result<Catalog, CatalogError>;
    pub fn get(&self, name: &str) -> Option<&dyn ErasedPlugin>;
    pub fn names(&self) -> Vec<&'static str>;
    /// Test-only: a catalog built from an explicit list, so a unit test never sees the whole binary.
    pub fn from_parts(parts: Vec<PluginRegistration>) -> Result<Catalog, CatalogError>;
}

// crates/bough-kernel/src/fiber.rs
bough_util::brand_id!(pub struct EntryId;);
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, serde::Serialize)] pub struct FiberUid(u64);

#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize)]
pub enum FiberState { Pending, Loading, Active, Unloading, Inactive, Failed }

#[derive(Clone)] pub struct FiberHandle { /* .. */ }
impl FiberHandle {
    pub fn uid(&self) -> FiberUid;
    pub fn id(&self) -> &EntryId;
    pub fn plugin(&self) -> Option<&'static str>;   // None for a pure group row (Decision D18)
    pub fn state(&self) -> FiberState;
    pub fn error(&self) -> Option<Arc<PluginError>>;
    /// Unmet required keys, empty unless PENDING.
    pub fn unmet(&self) -> Vec<String>;
    /// Awaits the end of any in-flight transition AND of the transition it is already targeting.
    pub async fn settled(&self) -> FiberState;
}
```

**Lifecycle, normative (§0.3).** `PENDING → LOADING → ACTIVE → UNLOADING → INACTIVE | FAILED`.

1. Each fiber has a driver task and a `target`. The reconciler only ever writes `target`. The
   driver runs a transition **to completion** before re-reading `target`: that is the inertia. A
   target that changes mid-transition is honoured after, not during.
2. A fiber's dependency target is
   `BTreeMap<&'static str, Option<ProviderUid>>` over its effective inject set, resolved in its
   realm and scope chain. PENDING while any *required* key resolves to `None`.
3. On LOADING: capture the **committed view** (an immutable snapshot of the resolved bindings),
   then run `apply`. Ok → ACTIVE. Err → FAILED, and the fiber's effects are unwound as if unloaded.
4. On UNLOADING: **first** remove every binding whose `ProviderUid.fiber` is this fiber and notify
   dependents; **then** await every notified dependent's own teardown; **only then** unwind this
   fiber's accumulator, LIFO. Dependents therefore recompute and tear down before any inverse of
   the provider runs (§0.3, verified by V10).
5. Nested mounts (`ctx.mount`, and the `group:` children of an entry) are effects of the parent, so
   step 4 cascades to them at their position in the accumulator.
6. A recompute that changes any resolved `ProviderUid` — a different provider fiber, or the same
   fiber's `republish` — is a **reload**: UNLOADING then LOADING. `ServiceSlot::set` does not change
   the `ProviderUid` and is therefore invisible.

### 2.8 Kernel: config tree, patch layers, expressions

```rust
// crates/bough-kernel/src/config/entry.rs
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub id: EntryId,
    #[serde(default)] pub plugin: Option<String>,           // None ⇒ a pure group row
    #[serde(default)] pub config: serde_yaml::Value,        // Null when absent; may contain !!expr
    #[serde(default)] pub disabled: Expr<bool>,             // literal bool or !!expr
    #[serde(default)] pub isolate: BTreeMap<String, RealmLabel>,  // service NAME -> realm
    #[serde(default)] pub inject: Inject,
    #[serde(default)] pub group: Vec<Entry>,
    #[serde(default)] pub include: Option<PathBuf>,         // grafted at parse time (Decision D19)
}

bough_util::brand_id!(pub struct RealmLabel;);

#[derive(Clone, Default, Debug, PartialEq, serde::Deserialize, serde::Serialize, schemars::JsonSchema)]
#[serde(from = "InjectRepr", into = "InjectRepr")]   // list ⇒ all required; map ⇒ {required, optional}
pub struct Inject { pub required: BTreeSet<String>, pub optional: BTreeSet<String> }
impl Inject {
    pub fn none() -> Self;
    pub fn required(keys: impl IntoIterator<Item = &'static str>) -> Self;
    pub fn optional(keys: impl IntoIterator<Item = &'static str>) -> Self;
    /// Entry ∪ plugin-static. The entry may ADD keys; it may not drop a plugin's static
    /// requirement (Decision D1).
    pub fn union(&self, other: &Inject) -> Inject;
    pub fn declares(&self, name: &str) -> bool;
}

// crates/bough-kernel/src/config/expr.rs — `!!expr` (§0.5), evaluated at mount
#[derive(Clone, Debug, PartialEq)]
pub enum Expr<T> { Literal(T), Source(String) }
impl<T: FromExprValue> Expr<T> { pub fn eval(&self, env: &ExprEnv) -> Result<T, ExprError>; }

pub struct ExprEnv { /* profile name, env snapshot, home dir */ }
impl ExprEnv {
    pub fn new(profile: &str) -> Self;
    pub fn with_var(self, k: &str, v: &str) -> Self;   // tests set variables without touching process env
}
/// Replaces every `!!expr` node in a `serde_yaml::Value` tree with its evaluated literal.
pub fn evaluate_tree(v: &serde_yaml::Value, env: &ExprEnv) -> Result<serde_yaml::Value, ExprError>;
```

Grammar, hand-rolled, deliberately tiny; the whole `expr.rs` is a recursive-descent parser plus an
evaluator over `ExprValue { Str, Num, Bool, Null }`:

```
expr := or
or   := and ("or" and)*
and  := not ("and" not)*
not  := "not" not | cmp
cmp  := atom (("==" | "!=") atom)?
atom := STRING | NUMBER | "true" | "false" | "null" | call | "(" expr ")"
call := IDENT "(" [ expr ("," expr)* ] ")"
```

Functions (Decision D10 — **no filesystem, no network, no clock**): `env(NAME)`,
`env_or(NAME, DEFAULT)`, `home_path(REL)`, `bough_path(REL)`, `platform()` (`"macos"`/`"linux"`),
`profile()`. Anything else is a parse error naming the unknown function.

```rust
// crates/bough-kernel/src/config/patch.rs — the id-keyed layer algorithm (§0.5), ~200 lines
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(from = "PatchRepr")]   // a bare YAML sequence ⇒ `insert` of those entries at root end
pub struct Patch { pub entries: BTreeMap<EntryId, EntryPatch>, pub insert: Vec<Insert>, pub remove: Vec<EntryId> }

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryPatch {
    /// REPLACES the whole config. No deep merge (§0.5): restate the fields you keep.
    #[serde(default)] pub config: Option<serde_yaml::Value>,
    #[serde(default)] pub plugin: Option<String>,
    #[serde(default)] pub disabled: Option<Expr<bool>>,
    #[serde(default)] pub isolate: Option<BTreeMap<String, RealmLabel>>,
    #[serde(default)] pub inject: Option<Inject>,
}

#[derive(Clone, Debug, serde::Deserialize)]
pub struct Insert { #[serde(flatten)] pub at: InsertAt, pub entry: Entry }
#[derive(Clone, Debug, serde::Deserialize)]
pub enum InsertAt { Before(EntryId), After(EntryId), Into(EntryId), #[serde(other)] RootEnd }

// crates/bough-kernel/src/config/compose.rs
bough_util::brand_id!(pub struct LayerId;);   // "bundle:bough-base", "profile:tui", "user", "--patch:0"

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)] pub struct Fingerprint(String); // sha256, hex

#[derive(Clone, Debug)]
pub struct Composition {
    pub tree: Vec<Entry>,                                  // after !!expr evaluation
    pub raw: Vec<Entry>,                                   // before evaluation, for the dump
    pub provenance: BTreeMap<EntryId, RowProvenance>,
    pub layers: Vec<LayerId>,
    pub warnings: Vec<ComposeWarning>,                     // e.g. a patch naming an absent row id
    pub fingerprint: Fingerprint,
}
#[derive(Clone, Debug)] pub struct RowProvenance { pub created_by: LayerId, pub fields: BTreeMap<&'static str, LayerId> }
#[derive(Clone, Debug)] pub enum ComposeWarning { AbsentRowId { layer: LayerId, id: EntryId } }

pub struct Composer { /* catalog, expr env */ }
impl Composer {
    pub fn new(catalog: &Catalog, env: ExprEnv) -> Self;
    pub fn layer(&mut self, id: LayerId, patch: Patch) -> &mut Self;
    /// Applies layers in order over an empty root, evaluates `!!expr`, then validates EVERY row:
    /// unknown plugin name ⇒ Err; a config the plugin's schema/`validate` rejects ⇒ Err. A patch
    /// naming an absent row id is a WARNING, not an error (§0.2).
    pub fn compose(self) -> Result<Composition, ComposeError>;
}

/// The one renderer. `--dump-config` prints `render(&composition)`; the V6 test prints
/// `render(&kernel.composition())`. There is no second formatter (Decision D9).
pub fn render(c: &Composition, format: DumpFormat) -> String;
#[derive(Clone, Copy)] pub enum DumpFormat { Yaml, Json }
```

**Fingerprint (Decision D9):** sha256 over the canonical JSON of `tree` — for every row, in tree
order: `id`, `plugin`, `config` (map keys sorted), `disabled` (resolved bool), `isolate`, `inject`,
then `group` recursively. Provenance, warnings and layer ids are excluded. It is computed on the
**evaluated** tree, so a row's config change, an `!!expr` result change, or a `disabled` flip all
move it; a comment or key reordering in the YAML does not.

**Reconciliation, normative (§0.3).** `Kernel::update` diffs `old.tree` against `new.tree` by id,
and per row:

| field changed | action |
|---|---|
| `id` or `plugin` | **rebuild**: dispose the old fiber entirely, create a new one (new `FiberUid`) |
| `config` | parse+validate, hand to the plugin via `reconfigure`; `Applied` ⇒ nothing, `Reload` ⇒ unload+load |
| `disabled` false→true | unload to INACTIVE (and cascade to `group` children) |
| `disabled` true→false | create/target the fiber; PENDING until its keys resolve |
| `isolate` | realm reassign ⇒ the committed view would change ⇒ reload |
| `inject` | recompute targets; reload iff a resolved `ProviderUid` differs |
| `group` | recurse; added children mount as effects of the parent, removed children dispose |
| row absent in new | dispose |
| row present only in new | create |

The quiescent state is a function of the final tree alone. The implementation guarantees this by
never acting on a diff directly: the diff writes each fiber's `target`, and the drivers converge.

### 2.9 Kernel: invariant runner

```rust
// crates/bough-kernel/src/invariant.rs
pub struct InvariantSpec {
    pub name: &'static str,
    pub plugin: &'static str,
    pub cadence: Cadence,
    pub check: fn(Context) -> futures::future::BoxFuture<'static, Result<(), InvariantViolation>>,
}
#[derive(Clone, Copy, Debug)] pub enum Cadence { OnQuiesce, Interval(Duration), OnEvent(&'static str) }
#[derive(Clone, Debug)] pub struct InvariantViolation { pub invariant: &'static str, pub plugin: &'static str, pub entry: EntryId, pub detail: String }
```

The runner is created iff `KernelOptions::invariants` is true (profiles `dev` and the test harness;
false in `tui` and `headless`). It collects the specs of every ACTIVE fiber's plugin, runs them at
their cadence, records violations in `Kernel::violations()`, and emits
`kernel/invariant-violated`. It never panics and never unloads anybody: a violation is a report.

### 2.10 Kernel: the handle

```rust
// crates/bough-kernel/src/kernel.rs
pub struct KernelOptions { pub profile: String, pub invariants: bool, pub reconcile_debounce: Duration }
pub struct Kernel { /* .. */ }
impl Kernel {
    pub fn new(catalog: Catalog, options: KernelOptions) -> Arc<Kernel>;
    pub fn root(&self) -> Context;
    pub async fn load(&self, c: Composition) -> Result<(), KernelError>;
    /// Live recompose. On Err the last good tree is untouched and `config-update-failed` has
    /// already been emitted (§0.3).
    pub async fn update(&self, c: Composition) -> Result<(), KernelError>;
    /// No fiber in Loading/Unloading and no pending reconcile. The workhorse of every test.
    pub async fn quiesce(&self);
    pub fn snapshot(&self) -> TreeSnapshot;
    pub fn composition(&self) -> Arc<Composition>;
    pub fn violations(&self) -> Vec<InvariantViolation>;
    /// Unload everything, LIFO, awaited. Teardown-before-exit (§0.1 item 2).
    pub async fn shutdown(&self);
}

#[derive(Clone, Debug, serde::Serialize)] pub struct TreeSnapshot { pub fingerprint: Fingerprint, pub rows: Vec<RowSnapshot> }
#[derive(Clone, Debug, serde::Serialize)]
pub struct RowSnapshot {
    pub id: EntryId, pub plugin: Option<String>, pub uid: Option<FiberUid>,
    pub state: FiberState, pub disabled: bool, pub unmet: Vec<String>,
    pub provides: Vec<&'static str>, pub realms: BTreeMap<String, RealmLabel>,
    pub children: Vec<RowSnapshot>,
}
impl TreeSnapshot { pub fn unresolved(&self) -> Vec<UnresolvedRow>; }   // enabled and not Active
```

`RowSnapshot` is what tests assert on: structural asserts, not rendered strings (AGENTS.md).

### 2.11 Errors

```rust
// crates/bough-kernel/src/error.rs
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    #[error("plugin `{plugin}` (row `{entry}`) read service `{key}` without declaring it in inject")]
    UndeclaredService { plugin: &'static str, entry: EntryId, key: &'static str },
    #[error("plugin `{plugin}` (row `{entry}`) read optional service `{key}`, which no active fiber provides")]
    ServiceUnavailable { plugin: &'static str, entry: EntryId, key: &'static str },
    #[error("row `{0}` is not in the tree")] NoSuchRow(EntryId),
    #[error("duplicate row id `{0}`")] DuplicateRowId(EntryId),
    #[error(transparent)] Compose(#[from] ComposeError),
}

#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("row `{entry}` names plugin `{plugin}`, which is not in the catalog")]
    UnknownPlugin { entry: EntryId, plugin: String, layer: LayerId },
    #[error("row `{entry}` (plugin `{plugin}`): {source}")]
    BadConfig { entry: EntryId, plugin: String, layer: LayerId, #[source] source: ConfigError },
    #[error("layer `{layer}`: {source}")] BadExpr { layer: LayerId, #[source] source: ExprError },
    #[error("layer `{layer}`: {0}")] BadYaml { layer: LayerId, .. },
    #[error("include `{path}` (from layer `{layer}`): {detail}")] BadInclude { .. },
}

#[derive(Debug, thiserror::Error)] pub enum ConfigError { Schema { .. }, Deserialize { .. }, Rejected { detail: String } }
#[derive(Debug, thiserror::Error)] pub struct PluginError { /* anyhow-backed, with the entry id attached */ }
```

The V8 message is normative, verbatim:
`plugin \`hello\` (row \`hello.greeter\`) read service \`ledger\` without declaring it in inject`.

### 2.12 Step types

Phase 0 defines **none**. Step types are the ledger's (§3) and arrive in Phase 1; the kernel is
domain-blind (§0.1 item 1) and a `step` type appearing in `bough-kernel` is a failed review. The
schema surface Phase 0 does own is `Plugin::Config` (schemars → JSON Schema, used by
`Composer::compose` for validation and by `--dump-config --schemas`) and the `Entry`/`Patch` serde
shapes above.

### 2.13 The launcher

```rust
// crates/bough/src/cli.rs
#[derive(clap::Parser)]
pub struct Cli {
    #[arg(long, default_value = "tui")] pub profile: String,
    /// Extra patch layers, applied last, in order.
    #[arg(long = "patch")] pub patches: Vec<PathBuf>,
    /// Print the composed tree and exit 0. Never mounts anything.
    #[arg(long)] pub dump_config: bool,
    #[arg(long, value_enum, default_value = "yaml")] pub dump_format: DumpFormat,
    /// Boot, quiesce, assert, tear down, exit. No TUI, no watch. Used by tests and audit-plugins.sh.
    #[arg(long)] pub check: bool,
    #[arg(long)] pub no_watch: bool,
    /// Override the embedded profiles/ + bundles/ directory.
    #[arg(long)] pub root: Option<PathBuf>,
}

// crates/bough/src/profile.rs
#[derive(serde::Deserialize)]
pub struct Profile { pub name: String, pub bundles: Vec<String>, #[serde(default)] pub invariants: bool, #[serde(default)] pub patch: Patch }
pub fn resolve_profile(name: &str, root: Option<&Path>) -> Result<(Profile, Sources), BootError>;

// crates/bough/src/compose.rs
/// The ONE composition path. `--dump-config` and boot both call it; that identity is what V6 tests.
pub fn compose_for(cli: &Cli, catalog: &Catalog) -> Result<Composition, BootError>;

// crates/bough/src/boot.rs
pub async fn boot(cli: Cli) -> Result<ExitCode, BootError>;
/// After quiesce: every row with `disabled == false` must be ACTIVE. Otherwise print each
/// unresolved row with its unmet keys, `kernel.shutdown().await`, and exit 1 (§0.2 "an enabled row
/// that never activates is a boot failure"; §0.1 "teardown-before-exit").
pub fn assert_all_activated(s: &TreeSnapshot) -> Result<(), BootError>;

// crates/bough/src/watch.rs
/// notify + debouncer on `bough_util::user_patch_path()`. On change: recompose and
/// `kernel.update(..)`. A failed recompose leaves the tree alone (the kernel already emitted
/// `config-update-failed`); the launcher logs it and keeps watching.
pub fn watch_user_patch(kernel: Arc<Kernel>, cli: Arc<Cli>) -> EffectHandle;
```

Layer order, normative (§0.5), and the order `LayerId`s appear in `Composition::layers`:

```
empty root
  → bundles/<b>.yml for each b in profile.bundles, in the profile's order   LayerId "bundle:<b>"
  → the profile's own `patch:` block                                        LayerId "profile:<name>"
  → ~/.bough/bough.patch.yml (absent ⇒ skipped silently)                    LayerId "user"
  → each --patch FILE, in argument order                                    LayerId "patch:<n>:<file>"
```

Profile/bundle lookup: `--root DIR`, else `$BOUGH_HOME/{profiles,bundles}` if present, else the
copies embedded with `include_dir!` (Decision D11 — note `include_dir`'s `files()` is **not**
recursive; use `find()`/`get_file()` with explicit paths).

Phase 0 ships:

```
profiles/tui.yml       bundles: [bough-base, bough-tui-app]   invariants: false   (default)
profiles/headless.yml  bundles: [bough-base, bough-headless]  invariants: false
profiles/dev.yml       bundles: [bough-base, bough-tui-app]   invariants: true
bundles/bough-base.yml       ONE row (the hello fixture's consumer + its default provider)
bundles/bough-tui-app.yml    exists, empty row list, a comment naming Phase 3
bundles/bough-headless.yml   exists, empty row list, a comment naming Phase 2
```

`bough-base.yml`, in full, is the Phase 0 composition:

```yaml
# bundles/bough-base.yml — REQUIREMENTS §0.5. Phase 0: the verification fixture only.
- id: greeting.provider
  plugin: greeting-echo
  config: { suffix: "" }
- id: hello.greeter
  plugin: hello
  config:
    who: world
    log_level: info
```

("ONE row" in §17 is satisfied in spirit by one *consumer* row; the provider row exists because a
tree with a consumer and no provider cannot boot — an enabled row that never activates is a boot
failure. Decision D14.)

### 2.14 The `hello` fixture

`plugins/hello` registers **three** catalog names (Decision D14, a deliberate Phase-0 exception to
one-crate-one-row; these exist only to prove the kernel and are deleted or demoted at Phase 8):

```rust
// plugins/hello/src/lib.rs
pub struct Greeting;                          // the Service Definition
impl ServiceKey for Greeting { type Value = GreetingHandle; const NAME: &'static str = "greeting"; }
#[derive(Clone)] pub struct GreetingHandle(pub Arc<dyn GreetingSink>);
pub trait GreetingSink: Send + Sync + 'static { fn greet(&self, who: &str) -> String; fn provider(&self) -> &'static str; }

pub struct EchoProvider;   // plugin "greeting-echo",  provides greeting; injects nothing
pub struct ShoutProvider;  // plugin "greeting-shout", provides greeting; injects nothing (SWAP target)
pub struct HelloPlugin;    // plugin "hello",          injects ["greeting"]

#[derive(serde::Deserialize, serde::Serialize, schemars::JsonSchema, PartialEq, Debug)]
pub struct HelloConfig {
    pub who: String,                                   // material ⇒ reload
    #[serde(default)] pub log_level: LogLevel,         // immaterial ⇒ absorbed by reconfigure
    /// Test hook: makes the invariant fail on purpose. V9's vehicle.
    #[serde(default)] pub plant_violation: bool,
    /// Test hook: `apply` reads a key it never declared. V8's vehicle.
    #[serde(default)] pub read_undeclared: Option<String>,
}
```

`hello`'s `reconfigure` returns `Applied` when only `log_level` differs and `Reload` otherwise —
that is what proves "config is handed to the plugin, which reloads only on a material diff".

`plugins/hello/src/invariant.rs`: **"every `hello/greeted` payload carries a seq strictly greater
than the previous one for the same fiber uid"** — an authoritative stream the plugin owns, per
§0.2. `plant_violation: true` makes `hello` emit a repeated seq, which is the planted violation V9
detects.

---

## 3. Work packages

Six packages, disjoint file sets. Order: **WP-1 → WP-2 → {WP-3, WP-4} → {WP-5, WP-6}**.

**Shared-file rule.** `crates/bough-kernel/src/lib.rs` and `crates/bough-kernel/Cargo.toml` belong
to **WP-2 alone**. WP-2 lands the complete `mod` list (including WP-3's and WP-4's modules, created
as empty files carrying only their invariant comment) and the complete dependency list. WP-3 and
WP-4 fill those files and never edit `lib.rs` or `Cargo.toml`. The root `Cargo.toml` belongs to
**WP-1 alone**; every later package uses `workspace = true` deps that WP-1 already pinned.

### WP-1 — workspace + `bough-util`

Files: `Cargo.toml` (root), `crates/bough-util/Cargo.toml`, `crates/bough-util/src/{lib,id,home,time}.rs`,
`.github/workflows/ci.yml` (one line).

Brief: pin the three new workspace deps (`parking_lot`, `include_dir`, `pin-project-lite`) with a
comment naming the row/seam that uses each, per the policy comment already in the root manifest.
Write `brand_id!` — a newtype over `Arc<str>` with Debug/Display/Clone/PartialEq/Eq/Hash/Ord/serde/
`FromStr`/`as_str`, and deliberately *no* `From<String>`, so a bare string cannot drift into an id
position. Write the home-path helpers, reading `$BOUGH_HOME` and `$HOME` on every call (never
cached in a `OnceLock`) so a test can point one test at one directory. Write `with_timeout` and
`Deadline`. No tokio runtime creation, no `tracing` init, no globals. Add `rebuild` to CI's push
branch list. This package must not grow: anything domain-shaped belongs in a plugin.

Tests: `id::tests::{brand_roundtrips_through_serde, brand_display_is_the_inner_string, brand_rejects_from_string_by_absence}`
(the third is a compile-fail note in the doc comment, not a test); `home::tests::{bough_home_honours_env, bough_home_defaults_under_home, user_patch_path_is_under_bough_home, bough_path_is_absolute}`;
`time::tests::{with_timeout_returns_the_value, with_timeout_reports_the_duration_it_waited, deadline_expires}`.

### WP-2 — kernel core: contexts, services, effects, events, scope, isolate/intercept

Files: `crates/bough-kernel/Cargo.toml`, `crates/bough-kernel/src/lib.rs`,
`crates/bough-kernel/src/{error,context,service,effect,event,scope}.rs`, plus empty stubs for
WP-3's and WP-4's modules.

Brief: the substrate everything else stands on, and the package with the subtlest semantics. Build
the binding store as `HashMap<(RealmLabel, &'static str, Option<ScopeKey>), Binding>` behind one
`parking_lot::RwLock`, with `Binding { uid: ProviderUid, value: Arc<dyn Any + Send + Sync> }`; all
async work happens outside the lock. Build `Context` as an `Arc` of {kernel, fiber uid, entry id,
plugin name, realm map, interception map, scope chain} — cloning is free and never re-owns.
Implement the committed view as an immutable `Arc<CommittedView>` handed to the context at
activation, and make `get::<K>()` consult the effective inject set *before* the store, so an
undeclared read is `UndeclaredService` even when the key happens to be bound. Implement effects
with an inverse accumulator per effect and a per-fiber accumulator of effects, both LIFO; `dispose`
claims the run with an `AtomicBool` and later callers await the same `Notify`. Implement the four
dispatch modes with per-listener containment (`catch_unwind` + `Err` logging) and `Next<E>`
consuming `self`. Implement scope: views down (nearest shadows farthest), admission up. Nothing in
this package knows what a fiber lifecycle or a config file is; it exposes the seams WP-3 drives.

Tests: `effect::tests::{inverses_unwind_lifo, effects_unwind_lifo_within_a_fiber, disposer_fires_at_most_once, concurrent_dispose_calls_await_one_run, dispose_halts_in_flight_effect_at_yield, halted_effect_still_unwinds_what_it_deferred}`;
`event::tests::{emit_runs_listeners_in_registration_order, waterfall_threads_the_value, waterfall_short_circuits_when_next_is_skipped, waterfall_prepend_runs_first, parallel_awaits_all_listeners, serial_returns_first_non_empty_in_order, serial_skips_later_listeners_after_a_hit, panicking_listener_is_contained_in_every_mode, contained_failure_emits_listener_failed}`;
`service::tests::{provide_binds_and_dispose_withdraws, undeclared_key_errors_at_point_of_use, undeclared_key_error_names_key_and_plugin, committed_view_survives_a_later_rebind, set_in_place_keeps_the_provider_uid, republish_bumps_the_provider_uid}`;
`scope::tests::{scoped_service_shadows_global_for_that_key_only, scoped_view_inherits_down_parent_chain, scoped_dispatch_admission_extends_up, scoped_dispatch_skips_sibling_and_descendant_scopes, disposing_a_scope_unwinds_its_registrations}`;
`context::tests::{isolate_gives_independent_bindings_per_realm, entries_sharing_a_realm_share_the_binding, intercept_metadata_is_visible_to_the_consumer, intercept_change_does_not_reload}`.

### WP-3 — kernel lifecycle: plugins, catalog, fibers, reconciler, invariant runner

Files: `crates/bough-kernel/src/{plugin,catalog,fiber,reconcile,kernel,invariant}.rs`.

Brief: turn a `Composition` into a running tree. Write the `Plugin` trait, the ZST `Shim<P>` and
the blanket `ErasedPlugin` impl; write `register_plugin!` and `Catalog::from_inventory` with a hard
error on duplicate names. Write the fiber driver: one task per fiber holding a `target`, running
each transition to completion before re-reading it — that loop *is* the inertia of §0.3, and the
temptation to short-circuit "we're about to unload anyway" is the bug it exists to prevent.
Implement UNLOADING in the mandated order: withdraw this fiber's bindings, notify dependents, await
their teardown, then unwind LIFO. Implement reload-on-`ProviderUid`-change (not on value change).
Write the reconciler as `diff(old, new) -> Vec<TargetWrite>` applied to fiber targets, never as
direct lifecycle calls, so the quiescent state is order-independent by construction. Write
`quiesce()` honestly: it must also cover fibers that a transition *created*. Write the invariant
runner (gated on `KernelOptions::invariants`), which reports and never unloads.

Tests: `fiber::tests::{pending_until_required_key_arrives, activation_captures_the_committed_view, reload_runs_to_completion_before_new_target, unload_runs_to_completion_before_a_reload_target, provider_stops_providing_before_its_inverses_run, dependents_tear_down_before_the_provider_unwinds, failed_apply_moves_to_failed_and_unwinds, group_children_are_effects_of_the_parent, unloading_a_parent_cascades_to_group_children}`;
`catalog::tests::{inventory_finds_registered_plugins, duplicate_catalog_name_is_an_error, from_parts_builds_an_isolated_catalog}`;
`reconcile::tests::{plugin_change_rebuilds_with_a_new_uid, id_change_rebuilds, material_config_diff_reloads, immaterial_config_diff_does_not_reload, config_is_handed_over_even_when_immaterial, disabled_true_unloads_and_cascades, disabled_false_reloads, isolate_change_reassigns_realm_and_reloads, inject_change_reloads_only_when_a_target_differs, quiescent_state_is_order_independent, removed_row_disposes}`;
`kernel::tests::{shutdown_unloads_everything_lifo, snapshot_reports_unmet_keys, update_failure_keeps_the_last_good_tree, update_failure_emits_config_update_failed}`;
`invariant::tests::{runner_reports_a_planted_violation, runner_is_inert_when_disabled, a_violation_does_not_unload_the_plugin}`.

### WP-4 — kernel config: entries, expressions, patch layers, composition

Files: `crates/bough-kernel/src/config/{mod,entry,expr,patch,compose,render}.rs`.

Brief: pure functions over YAML — no kernel state, no tokio, no I/O beyond reading the files an
`include:` names. Write `Entry` with `deny_unknown_fields` (a typo in a bundle must be loud), the
`Inject` list-or-map repr, and `include:` grafting at parse time so later layers can patch included
ids. Write the `!!expr` parser and evaluator against the grammar in §2.8, with an `ExprEnv` that
carries its own variable map so tests never mutate the process environment. Write the patch
algorithm: id-keyed field replacement with **no deep merge whatsoever** — `config` is replaced
wholesale, and if that ever feels inconvenient the answer is to restate the fields, not to merge.
A patch naming an absent id is a `ComposeWarning`, never an error. Write `Composer`, which validates
every row against its plugin's schema and `validate()` and returns `Err` for the whole candidate on
the first bad row, and the canonical fingerprint. Write `render()` — the single formatter both
`--dump-config` and the V6 test use.

Tests: `entry::tests::{entry_roundtrips, unknown_field_is_rejected, inject_list_form, inject_map_form, include_is_grafted_at_parse_time, include_cycle_is_an_error}`;
`expr::tests::{env_lookup, env_or_default, home_path_is_absolute, platform_matches_the_host, not_and_or_precedence, equality_on_strings, unknown_function_is_a_parse_error, expr_in_a_nested_config_value_is_evaluated, literal_bool_needs_no_expr}`;
`patch::tests::{config_is_replaced_not_merged, disabled_can_be_set_by_patch, insert_before_after_into_and_root_end, remove_drops_the_row_and_its_group, absent_row_id_is_a_warning_not_an_error, bare_sequence_is_sugar_for_insert_at_root}`;
`compose::tests::{layers_apply_in_order, provenance_names_the_last_writing_layer_per_field, unknown_plugin_name_is_a_compose_error, bad_config_is_a_compose_error_naming_the_row, fingerprint_is_stable_across_key_order_and_comments, fingerprint_changes_when_a_row_config_changes, fingerprint_changes_when_disabled_flips, fingerprint_is_computed_after_expr_evaluation}`;
`render::tests::{render_is_deterministic, render_annotates_each_row_with_its_layer}`.

### WP-5 — the launcher

Files: `crates/bough/Cargo.toml`, `crates/bough/src/{main,cli,profile,compose,boot,watch}.rs`,
`profiles/{tui,headless,dev}.yml`, `bundles/{bough-base,bough-tui-app,bough-headless}.yml`,
`scripts/audit-plugins.sh`.

Brief: composition and teardown, and nothing else — a behaviour that lives here instead of in a
plugin row is a §0.1 violation. Resolve the profile (`--root`, then `$BOUGH_HOME`, then the
`include_dir!` copies), stack the layers in the order of §2.13, and mount. `--dump-config` and boot
must call **one** function, `compose_for`, and the dump must be `render()` of exactly the
`Composition` that boot hands the kernel; that identity is the whole point of V6, so do not add a
second pretty-printer. After `quiesce()`, assert every enabled row is ACTIVE and, on failure, print
each unresolved row with its unmet keys, `shutdown().await`, and exit 1 — teardown before exit, so
a Phase-3 TUI failure still restores the terminal. Watch `~/.bough/bough.patch.yml` with
notify + debouncer and call `kernel.update`; a rejected candidate is logged and watching continues.
Handle SIGINT as `shutdown().await` then exit.

Tests: `profile::tests::{resolve_embedded_profile, root_overrides_embedded, unknown_profile_names_the_search_path}`;
`compose::tests::{layer_order_matches_requirements, user_patch_absent_is_not_an_error, cli_patches_apply_last_in_argument_order}`;
`boot::tests::{assert_all_activated_passes_on_a_full_tree, assert_all_activated_names_every_unresolved_row_and_its_unmet_keys, disabled_rows_are_not_required_to_activate}`;
`crates/bough/tests/dump_config.rs::{dump_config_equals_the_booted_tree, dump_config_annotates_the_last_writing_layer, dump_config_exits_zero_without_mounting, fingerprint_changes_when_a_row_config_changes}`;
`crates/bough/tests/boot.rs::{enabled_row_that_never_activates_fails_boot_after_teardown, boot_failure_exit_code_is_one, sigint_tears_down_before_exit}`.

### WP-6 — the `hello` fixture and the Phase 0 verification suite

Files: `plugins/hello/Cargo.toml`, `plugins/hello/src/{lib,provider,invariant}.rs`,
`plugins/hello/tests/lifecycle.rs`, `crates/bough/tests/{swap,bad_patch,invariants}.rs`,
`crates/bough/tests/support/mod.rs`.

Brief: the fixture is a *test instrument*, so it is written to be observed: every interesting
moment pushes a line onto a shared `Vec<String>` trace (`("hello", "apply")`,
`("greeting-echo", "withdraw")`, …) that the tests assert on in order. Three registrations:
`hello` (consumer, `inject: ["greeting"]`), `greeting-echo`, `greeting-shout` (providers of the
same key with *equal* greeting output, which is what makes V2 meaningful: the reload happens
because the provider fiber changed, not because the value did). `HelloConfig::log_level` is the
immaterial field; `who` is the material one. `read_undeclared` and `plant_violation` are the two
test hooks, each used by exactly one V-bullet. `src/invariant.rs` holds the real invariant
(monotonic `hello/greeted` seq per fiber uid) — not a placeholder, since Phase 8 audits these. The
integration tests live here because they need a real catalog and a real kernel; `tests/support`
gives them `boot_with(yaml) -> (Arc<Kernel>, TempDir)` and a patch-file writer.

Tests: `plugins/hello/tests/lifecycle.rs::{hello_stays_pending_until_greeting_is_provided_by_an_active_fiber, hello_activates_when_the_provider_activates, hello_unloads_when_the_provider_withdraws, hello_reloads_when_a_different_fiber_provides_an_equal_value, provider_in_place_set_is_not_observed_by_hello, hello_effects_unwind_lifo_on_unload, unloading_a_parent_cascades_to_nested_mounts}`;
`plugins/hello/tests/undeclared.rs::hello_reading_undeclared_key_names_key_and_plugin`;
`plugins/hello/src/invariant.rs::tests::{greeted_seq_is_monotonic, planted_violation_is_detected}`;
`crates/bough/tests/bad_patch.rs::{invalid_config_leaves_last_good_tree_and_broadcasts_failure, unknown_plugin_name_leaves_last_good_tree, patch_naming_absent_row_id_is_a_warning_and_the_tree_still_updates}`;
`crates/bough/tests/invariants.rs::{planted_violation_is_reported_in_the_dev_profile, invariant_runner_is_silent_in_the_tui_profile}`;
`crates/bough/tests/swap.rs::{patch_swaps_the_provider_row_and_hello_reloads_against_it, swapped_out_provider_leaves_no_listeners_and_no_bindings, dump_config_reflects_the_swapped_row, disabling_the_provider_leaves_hello_pending_and_the_rest_unchanged}`.

---

## 4. Verification map

Every bullet of the phase brief, to the test that proves it. A bullet is not "done" until the named
test has run green; a bullet whose test is skipped or `#[ignore]`d is not done.

| # | claim | test(s) |
|---|---|---|
| **V1** | hello registers through inventory, declares `inject: ["greeting"]`, activates ONLY when an ACTIVE fiber provides it; stays PENDING before | `plugins/hello/tests/lifecycle.rs::hello_activates_when_the_provider_activates` (boots from `Catalog::from_inventory()` and mounts plugin `hello`, which is `UnknownPlugin` unless the inventory registration linked — `catalog::tests::inventory_finds_registered_plugins` proves only the mechanism, on a kernel-internal fixture) · `plugins/hello/tests/lifecycle.rs::hello_stays_pending_until_greeting_is_provided_by_an_active_fiber` · `…::hello_activates_when_the_provider_activates` · `fiber::tests::pending_until_required_key_arrives` |
| **V2** | unloads on withdraw; reloads when a *different fiber* provides an *equal* value; an in-place overwrite is not observed | `plugins/hello/tests/lifecycle.rs::hello_unloads_when_the_provider_withdraws` · `…::hello_reloads_when_a_different_fiber_provides_an_equal_value` · `…::provider_in_place_set_is_not_observed_by_hello` · `service::tests::set_in_place_keeps_the_provider_uid` · `service::tests::republish_bumps_the_provider_uid` · `kernel::e2e::republish_reloads_an_active_dependent_and_set_does_not` (the propagation itself, not just the moved uid) |
| **V3** | effects unwind LIFO on unload; nested mounts cascade; a disposer fires at most once and halts an in-flight effect at its next yield boundary | `effect::tests::inverses_unwind_lifo` · `effect::tests::effects_unwind_lifo_within_a_fiber` · `effect::tests::disposer_fires_at_most_once` · `effect::tests::concurrent_dispose_calls_await_one_run` · `effect::tests::dispose_halts_in_flight_effect_at_yield` · `fiber::tests::unloading_a_parent_cascades_to_group_children` · `plugins/hello/tests/lifecycle.rs::unloading_a_parent_cascades_to_nested_mounts` · `plugins/hello/tests/lifecycle.rs::hello_effects_unwind_lifo_on_unload` |
| **V4** | waterfall skipping `next()` short-circuits; `prepend: true` runs first; parallel awaits fan-out; serial returns the first non-empty in order; a throwing listener is contained | `event::tests::waterfall_short_circuits_when_next_is_skipped` · `event::tests::waterfall_prepend_runs_first` · `event::tests::parallel_awaits_all_listeners` · `event::tests::serial_returns_first_non_empty_in_order` · `event::tests::serial_skips_later_listeners_after_a_hit` · `event::tests::panicking_listener_is_contained_in_every_mode` · `event::tests::waterfall_threads_the_value` · `event::tests::emit_runs_listeners_in_registration_order` · `event::tests::contained_failure_emits_listener_failed` |
| **V5** | a scoped registration shadows its global twin for one scope key only; views inherit down, admission extends up | `scope::tests::scoped_service_shadows_global_for_that_key_only` · `scope::tests::scoped_view_inherits_down_parent_chain` · `scope::tests::scoped_dispatch_admission_extends_up` · `scope::tests::scoped_dispatch_skips_sibling_and_descendant_scopes` · `scope::tests::disposing_a_scope_unwinds_its_registrations` |
| **V6** | `bough --profile tui --dump-config` equals what boots — same tree, per-row layer annotations, same fingerprint — and the fingerprint moves when a row's config changes | `crates/bough/tests/dump_config.rs::dump_config_equals_the_booted_tree` · `…::dump_config_annotates_the_last_writing_layer` · `…::fingerprint_changes_when_a_row_config_changes` · `compose::tests::fingerprint_is_stable_across_key_order_and_comments` · `compose::tests::fingerprint_changes_when_disabled_flips` · `compose::tests::fingerprint_is_computed_after_expr_evaluation` · `crates/bough/tests/dump_config.rs::dump_config_exits_zero_without_mounting` · `config::compose::tests::a_row_naming_no_plugin_is_rejected_by_the_composer` (the dump and the mount path agree about a plugin-less row) |
| **V7** | a bad patch leaves the last good tree running and broadcasts `config-update-failed`; an absent row id is a warning; an enabled row that never activates fails boot with teardown | `crates/bough/tests/bad_patch.rs::invalid_config_leaves_last_good_tree_and_broadcasts_failure` · `…::unknown_plugin_name_leaves_last_good_tree` · `…::patch_naming_absent_row_id_is_a_warning_and_the_tree_still_updates` · `kernel::tests::update_failure_keeps_the_last_good_tree` · `kernel::tests::update_failure_emits_config_update_failed` · `kernel::tests::a_rejected_reconfigure_touches_nothing` (the KERNEL-side rejection: a candidate that composes and is then refused at mount) · `crates/bough/tests/watch_broadcast.rs::a_patch_that_stops_composing_broadcasts_and_leaves_the_tree_running` · `crates/bough/tests/boot.rs::enabled_row_that_never_activates_fails_boot_after_teardown` · `crates/bough/tests/boot.rs::boot_failure_exit_code_is_one` · `crates/bough/tests/boot.rs::a_failed_boot_unwinds_the_rows_that_did_activate` · `crates/bough/tests/boot.rs::sigint_tears_down_before_exit` · `kernel::tests::a_live_recompose_broadcasts_rows_that_never_activated` (Decision D12's runtime half) |
| **V8** | reading an undeclared service key fails at the point of use, naming the key and the plugin | `service::tests::undeclared_key_errors_at_point_of_use` · `service::tests::undeclared_key_error_names_key_and_plugin` · `plugins/hello/tests/undeclared.rs::hello_reading_undeclared_key_names_key_and_plugin` |
| **V9** | the invariant runner reports one planted violation in dev/test and is silent in tui | `crates/bough/tests/invariants.rs::planted_violation_is_reported_in_the_dev_profile` · `…::invariant_runner_is_silent_in_the_tui_profile` · `crates/bough/tests/invariants.rs::a_clean_tree_reports_nothing_in_the_dev_profile` · `invariant::tests::runner_is_inert_when_disabled` · `invariant::tests::an_undispatched_cadence_is_reported_and_does_not_run` · `bough_plugin_hello::invariant::tests::{greeted_seq_is_monotonic, planted_violation_is_detected, forgetting_a_fiber_lets_a_reload_start_over}` |
| **V10** | the lifecycle is inertial; a provider stops providing before any inverse runs; per-field reconciliation behaves as §0.3 and the quiescent state is order-independent | `fiber::tests::reload_runs_to_completion_before_new_target` · `fiber::tests::unload_runs_to_completion_before_a_reload_target` · `fiber::tests::provider_stops_providing_before_its_inverses_run` · `fiber::tests::dependents_tear_down_before_the_provider_unwinds` · `reconcile::tests::plugin_change_rebuilds_with_a_new_uid` · `reconcile::tests::material_config_diff_reloads` · `reconcile::tests::immaterial_config_diff_does_not_reload` · `reconcile::tests::config_is_handed_over_even_when_immaterial` · `reconcile::tests::disabled_true_unloads_and_cascades` · `reconcile::tests::disabled_false_reloads` · `reconcile::tests::isolate_change_reassigns_realm_and_reloads` · `reconcile::tests::inject_change_reloads_only_when_a_target_differs` · `reconcile::tests::removed_row_disposes` · `reconcile::tests::quiescent_state_is_order_independent` · `reconcile::tests::disabling_and_reconfiguring_in_one_update_still_hands_the_config_over` · `reconcile::tests::a_disabling_update_that_also_changes_config_emits_both_writes` · `reconcile::tests::removing_a_provider_does_not_resurrect_a_dependent_disabled_in_the_same_update` · `fiber::tests::a_reload_request_never_resurrects_a_disabled_fiber` · `fiber::tests::a_panicking_apply_fails_the_fiber_instead_of_wedging_the_kernel` |
| **SWAP** | a live patch edit replaces the provider row with a second plugin providing the same key: hello reloads against it, the old provider leaks nothing, `--dump-config` reflects it; a second edit disabling the provider leaves hello PENDING and nothing else changed | `crates/bough/tests/swap.rs::patch_swaps_the_provider_row_and_hello_reloads_against_it` · `…::swapped_out_provider_leaves_no_listeners_and_no_bindings` · `…::dump_config_reflects_the_swapped_row` · `…::disabling_the_provider_leaves_hello_pending_and_the_rest_unchanged` |

**How the SWAP test runs, concretely** (it is the phase's exit gate, so its shape is fixed here):
boot `--profile tui` with `BOUGH_HOME` pointed at a `TempDir` **through the launcher's own
`bough::compose::compose_plan`**, over a `profiles/` and `bundles/` laid out inside that home — so
the normative §0.5 layer stack (bundles → profile patch → user patch → `--patch`) is what the gate
exercises. `quiesce()`; assert `hello.greeter` is ACTIVE against provider `greeting-echo`. Write
`{ entries: { greeting.provider: { plugin: greeting-shout } } }` into
`$BOUGH_HOME/bough.patch.yml`; recompose through **`bough::watch::recompose_once`**, the very
function the debounced watch task calls, and `quiesce()`. Assert: hello's
`FiberUid` is UNCHANGED (a reload keeps the fiber; only `plugin`/`id` rebuilds) and the trace shows
its re-`apply` against the new provider, the trace shows `greeting-echo:unload` strictly before
`hello:apply`, the store holds exactly one `greeting` binding and its `ProviderUid.fiber` is the
shout fiber, the listener registry holds no listener owned by the echo fiber, and the fingerprint
moved. Then append `disabled: true` to that row; `quiesce()`; assert `greeting.provider` is
INACTIVE, `hello.greeter` is PENDING with `unmet == ["greeting"]`, and every other row's
`FiberUid` is unchanged. No recompile, no restart, in one test process. The only thing the gate
does not drive is the `notify` debouncer itself; that is covered by
`crates/bough/tests/watch_broadcast.rs`, which writes a real file and waits for the real watch.

---

## 5. What Phase 0 does NOT build

Stated so a reviewer does not read an omission as a gap: no ledger, no agent, no model call, no
tool, no TUI, no HTTP surface (§17: `bough-server` and jungler's HTTP surface are retired), no
step types (§2.12), no `tokio-cron-scheduler`, no `rhai`, no `notify` beyond the single user-patch
watch, no hot-lib-reloader (§13 says verify it parses Rust 2024 `unsafe(no_mangle)` first; that
verification is not Phase 0 work), no `cargo xtask` event catalog gate (§15 item 7 says not before
Phase 2), and no adoption of `cordis-core` (§15 item 5 says decide at Phase 4).

---

## 6. Decisions taken where REQUIREMENTS is silent

Each is a real choice with a real alternative; each is cheap to revisit.

- **D1 — inject union.** A row's effective inject set is `Plugin::inject() ∪ entry.inject`. The
  entry may add keys (including marking one optional); it may not drop a plugin's static
  requirement. *Alternative:* entry-overrides-plugin, rejected because it lets a bundle patch
  disarm a plugin's own contract.
- **D2 — inject YAML shape.** `inject: [a, b]` means both required; `inject: {required: [a],
  optional: [b]}` is the long form. §0.3 shows only the list.
- **D3 — four event traits, not one trait plus a mode enum.** §0.2 says dispatch mode is part of the
  public contract, so the compiler enforces it. Cost: no single `Event` supertrait to iterate over
  for the §15 item 7 catalog gate — hence the `MODE` const on each trait.
- **D4 — containment semantics per mode.** A contained listener failure is: skipped
  (`emit`/`parallel`), `None` (`serial`), *delegate unchanged* (`waterfall`), and always emits
  `kernel/listener-failed`. §0.3 says only "contained".
- **D5 — `ServiceKey::Value` is Sized.** Trait-object services are exposed as a concrete handle
  newtype owned by the Service Definition (`GreetingHandle(Arc<dyn GreetingSink>)`). *Alternative:*
  an unsized `Value` with a bespoke downcast, rejected as unsafe-adjacent for no gain.
- **D6 — provider identity is `ProviderUid { fiber, seq }`.** §0.3 says "provider fiber uid", but
  also demands that withdraw-and-re-provide by the *same* fiber propagates; a bare fiber uid cannot
  express both. `seq` is bumped per `provide`/`republish` and left alone by `set`.
- **D7 — `Reconfigure { Applied, Reload }`, plugin-decided, defaulting to `old != new`.** This is
  the concrete spelling of "config is handed to the plugin, which reloads only on a material diff".
- **D8 — patch document shape.** `{ entries: {id: {..}}, insert: [..], remove: [..] }`, with a bare
  YAML sequence as sugar for inserting those entries at the root end (so `bough-base.yml` reads as
  a plain row list). `remove:` is not in §0.5; it is here because a profile must be able to drop a
  base row without knowing what `disabled` would leave behind. Still no deep merge, ever.
- **D9 — fingerprint is post-`!!expr`.** Hashing the evaluated tree means "the tree that was live"
  in the §0.5 sense, and an env change that changes behaviour changes the fingerprint. Cost: the
  fingerprint is not reproducible from the YAML alone; `--dump-config` therefore prints both the
  raw expression and its resolved value.
- **D10 — expression functions are pure.** `env`, `env_or`, `home_path`, `bough_path`, `platform`,
  `profile`. No `exists()`, no shell-out: a config expression that touches the filesystem makes
  `--dump-config` lie on a different machine.
- **D11 — profiles/bundles are embedded with `include_dir!` and overridable** by `--root`, then
  `$BOUGH_HOME/{profiles,bundles}`. An installed binary must boot with no repo checked out.
  (Known trap from the old tree: `include_dir`'s `files()` is not recursive.)
- **D12 — unresolved rows: fatal at boot, loud at runtime.** §0.2's "enabled row that never
  activates is a boot failure" is applied literally at boot. During a live recompose the candidate
  has already validated, so an unresolved row emits `kernel/rows-unresolved` and logs at WARN; the
  tree stays. Killing a running tree over a live edit would be a worse failure mode.
- **D13 — one tokio runtime; every plugin `Send + Sync + 'static`.** A per-fiber driver task gives
  the inertial lifecycle; one reconciler task serialises target writes. A `!Send` surface (the TUI)
  owns its own thread behind a `Send` handle.
- **D14 — `plugins/hello` registers three catalog names** (`hello`, `greeting-echo`,
  `greeting-shout`), a deliberate exception to one-crate-one-row for a fixture whose only job is to
  prove the kernel. `bundles/bough-base.yml` therefore has two rows in Phase 0, not the literal one
  of §17: a consumer row with no provider cannot boot, by §0.2's own rule.
- **D15 — `--check`.** Boot, quiesce, assert, tear down, exit. Not in REQUIREMENTS; every
  integration test and `scripts/audit-plugins.sh` (§17 Phase 8) needs exactly this and would
  otherwise each invent it.
- **D16 — `plant_violation` / `read_undeclared` config hooks on `hello`.** Test hooks living in the
  fixture rather than in test-only plugin crates, so V8 and V9 exercise the real catalog path.
- **D17 — CI gains `rebuild` in `on.push.branches`.** The only CI edit in Phase 0.
- **D18 — a row may omit `plugin:`.** Such a row is a pure group: it owns children, an `isolate:`
  map and an `inject:` set, and is ACTIVE as soon as it is mounted. §0.3 lists `group` as a field of
  an ordinary entry without saying whether `plugin` stays mandatory.
- **D19 — `include:` is resolved at parse time**, before any patch layer, so a later layer can
  patch an included row by id. *Alternative:* graft after patching, rejected because it makes
  included rows unpatchable.
- **D20 — the reqwest 0.12/0.13 stance**, recorded in §1.1 above: it stands, it stays confined to
  `bough-llm` (0.12) and later `rmcp` (0.13), and Phase 0 adds no reqwest dependency.

## Integration record (what the merge of WP-1..WP-6 actually changed)

Six defects sat on the seams between packages. Each is fixed in the file that owned the bug, and
each was pinned by a test that was already written and red.

- **The kernel handle never reached a row's context.** `Context::with_kernel` derived a *new*
  `Context`, so the `PluginFactory`'s copy of the root — taken before `Arc<Kernel>` existed — kept
  `kernel: None` and every `ctx.mount()` panicked. The handle now lives in a
  `Arc<OnceLock<Weak<Kernel>>>` shared by every context derived from the root, filled once by
  `Kernel::assemble`. `Context::kernel()` returns `Arc<Kernel>` (it had no external callers).
  Pinned by `bough-plugin-hello` `tests/lifecycle.rs::unloading_a_parent_cascades_to_nested_mounts`.
- **A reconfigured body was never attached.** `PluginFactory::reconfigure` attached the running
  fiber's context to the new body only on the `Applied` verdict, so a `Reload` verdict — and every
  `TargetWrite::Retarget` — reached `PluginBody::load` with no context and panicked
  `attached before load`. Attach is now unconditional on both paths: a reconfigure never
  re-identifies the fiber. Pinned by `bough` `tests/bad_patch.rs::patch_naming_absent_row_id_...`.
- **`normalize_expr_tags` walked bytes, not chars**, turning any multi-byte character (the em dash
  in `bundles/bough-base.yml`'s first comment) into Latin-1 garbage that serde_yaml then rejected
  as a control character. The whole launcher was unbootable. Pinned by
  `config::expr::tests::non_ascii_survives_normalisation`.
- **The invariant runner was never created.** `Kernel::start_invariants` had no caller outside the
  kernel's own tests, so `violations()` was always empty under the `dev` profile. It is now called
  from `Kernel::assemble`: the runner is a property of `KernelOptions::invariants`, not of a caller
  remembering to ask. Pinned by `bough` `tests/invariants.rs`.
- **Nested mounts were invisible.** `Kernel::snapshot` walked only the config tree, so a row mounted
  through `ctx.mount()` existed as a fiber but in no snapshot. `row_snapshot` now appends the
  fiber's runtime children (recursively) after its configured `group`.
- **The reload/rebuild uid question is settled against §0.3 line 107**: `plugin`/`id` *rebuilds*,
  everything else reconciles in place, so a RELOAD keeps the `FiberUid`. `tests/swap.rs` asserted
  the opposite and now asserts uid stability plus the reload on the ordered trace, which is what
  `lifecycle.rs` already did.

Also removed at integration: the `#![allow(unused_variables, dead_code)]` scaffold on
`bough-kernel` and the six dead items it was hiding (`effect::EffectResult`, `Fiber::parent` and
its accessor, `kernel::plugin_error`, `service::Store::get`, `TreeHarness::store`), and the unused
`pin-project-lite` pin (`EffectCtx::checkpoint` is a plain atomic + `yield_now`).

---

## 7. Deviations and open items (closing review, Phase 0)

What the closing review found, what was fixed, and what was deliberately left. Everything under
"Fixed" has a named test in section 4 or below it; everything under "Open" does not, and says so.

### Fixed in the closing pass

| Was | Now | Pinned by |
|---|---|---|
| `diff_row` returned early on `disabled: false→true`, swallowing a `config`/`isolate`/`inject` change arriving in the same update; the row's bookkeeping took the new config anyway, so re-enabling it later loaded the STALE config. Two orders, two quiescent states. | The `Unload` write no longer short-circuits the rest of the row's diff. | `reconcile::tests::disabling_and_reconfiguring_in_one_update_still_hands_the_config_over` · `…::a_disabling_update_that_also_changes_config_emits_both_writes` |
| `Fiber::request_reload` set `want = true`, so a provider's teardown poke RESURRECTED a dependent the same update had disabled — silently, because `unresolved()` skips disabled rows. | `request_reload` bumps the generation only. `want` is written by the reconciler's `disabled` decision alone (`set_want`). | `reconcile::tests::removing_a_provider_does_not_resurrect_a_dependent_disabled_in_the_same_update` · `fiber::tests::a_reload_request_never_resurrects_a_disabled_fiber` |
| `ServiceSlot::republish` moved the `ProviderUid` and nothing recomputed anybody: §0.3's only documented propagation mechanism did not propagate. | Every binding-store mutation calls `KernelCore::bindings_changed`, which re-resolves each ACTIVE fiber's targets and requests a reload wherever a resolved `ProviderUid` moved. Synchronous, so `quiesce()` waits for the reload. | `kernel::e2e::republish_reloads_an_active_dependent_and_set_does_not` (asserts BOTH halves: `set` does not propagate, `republish` does) |
| `include:` was dead on the production path — `Patch::parse` never grafted, `evaluate_rows` then dropped the field. A row naming a nonexistent include composed cleanly. | `Patch::parse_in(yaml, base, layer)` grafts every include relative to the directory the document was read from; the launcher passes a real base per layer (embedded documents resolve against `$BOUGH_HOME`). A missing or cyclic include is a `ComposeError` naming the path. | `crates/bough/tests/include.rs::{a_user_patch_include_is_grafted_into_the_composed_tree, a_missing_include_is_an_error_not_a_skipped_row}` (real binary) · `config::patch::tests::parse_in_grafts_an_include` |
| A plugin that panicked in `apply` killed the driver task with `busy == true`, so the fiber never settled and `Kernel::shutdown()` blocked forever — the teardown-before-exit guarantee. | `apply`, `withdraw` and `unwind` run under `catch_unwind`; a panic in `apply` is a `PluginError` and the fiber rests FAILED. `FiberRuntime::dispose` waits at most `DISPOSE_CEILING` (5s) and says so at ERROR. | `fiber::tests::a_panicking_apply_fails_the_fiber_instead_of_wedging_the_kernel` |
| `quiesce_runtime` returned AS IF quiescent after its 10s ceiling; no caller could tell success from timeout, so under load the whole suite asserted on an unconverged tree. | It returns `bool` (`#[must_use]`) and names the still-converging rows in the timeout log. `boot()` treats a non-quiescent tree as a boot failure; the launcher harness asserts on it. | `crates/bough/tests/boot.rs` (boot path) · the harness's `assert!(kernel.quiesce().await)` |
| `kernel/rows-unresolved` was declared and never emitted: Decision D12's runtime half did not exist. After a live recompose, an enabled row that never activated was reported by nothing. | `update_tree` reports unresolved rows at WARN and broadcasts `RowsUnresolved` through `KernelEvents::rows_unresolved`. | `kernel::tests::a_live_recompose_broadcasts_rows_that_never_activated` |
| The composer SKIPPED a row with no `plugin:` while the mount path rejected it, so `--dump-config` exited 0 on a tree that could not boot. | `ComposeError::MissingPlugin`: the composer rejects it too, naming Decision D18. `Entry::plugin` and `FiberHandle::plugin` no longer document D18 as if it worked. | `config::compose::tests::a_row_naming_no_plugin_is_rejected_by_the_composer` |
| `update_tree` ran `PluginFactory::reconfigure` inside the APPLY loop — the one unvalidated call site — and returned `Err` from the middle of it, leaving a half-applied tree, a stale recorded tree and, on the next diff, a leaked fiber. | The reconfigure is computed and validated in the first pass with everything else; the apply loop only installs the already-validated result. | `kernel::tests::a_rejected_reconfigure_touches_nothing` |
| `Context::resolve` fell through to the live store for any name the committed view had no entry for, so an OPTIONAL key absent at activation was read live for the rest of the fiber's life. | `CommittedView` records every name it was captured FOR; a declared name is answered by the view alone, `None` included. Only a key the fiber provides itself reaches the live store. | `service::tests::an_optional_key_absent_at_activation_stays_absent_for_this_life` |
| `check_declared`'s "a fiber may read what it itself provides" allowance consulted the LIVE store, which UNLOADING empties as step 1 — so a disposer reading its own key got `UndeclaredService`, a capability error. | The allowance is answered from a per-fiber record of what the fiber provided during its current life, cleared when it loads again. | `service::tests::reading_a_self_provided_key_after_withdrawal_is_unavailable_not_undeclared` |
| `KernelCore::unwind_fiber` removed the accumulator before unwinding it while `register_effect` used `or_insert_with`, so an effect registered from inside an inverse recreated an accumulator nobody would ever unwind. | A tombstone marks the fiber unwound; a registration arriving then is disposed instead of resurrecting the accumulator. The tombstone is cleared at the top of each load, because a RELOAD keeps the `FiberUid`. | (covered indirectly by the LIFO/cascade suite; no dedicated test — see Open items) |
| `Cadence::Interval` / `Cadence::OnEvent` were accepted from a plugin and then silently never run. | `collect_specs` logs each undispatched cadence at WARN and records it in `InvariantRunner::unsupported()`. Still not dispatched — the silence is what was fixed. | `invariant::tests::an_undispatched_cadence_is_reported_and_does_not_run` |
| The UNLOADING dependent wait abandoned §0.3's mandated order after 5s with no log, no event and no error. | The expiry logs at ERROR naming the provider and the dependent. The ceiling itself stays (see Open items). | — (a timing path; the log is the fix) |
| `WatchHandle::stop`/`Drop` called `task.abort()`, which could land inside `update_tree` — not cancellation-safe — on the NORMAL SIGINT path. | `stop()` is async and cooperative: drop the watcher, the channel closes, the loop finishes its in-flight recompose and returns. No `abort()`, no `Drop` impl. | `crates/bough/tests/watch_broadcast.rs` · `crates/bough/tests/boot.rs::sigint_tears_down_before_exit` |
| `watch_user_patch` panicked (`.expect`) when the OS refused a watcher, on a path whose whole purpose is that a failure there never disturbs the tree. | It warns and returns an inert handle; the run continues without live reload. | — (an OS-failure path; not reproducible hermetically) |
| `recompose_once` dropped `Composition::warnings`, so an absent row id in an edited user patch was reported at boot and nowhere else. | Both paths call `boot::report_warnings`. | `crates/bough/tests/bad_patch.rs::patch_naming_absent_row_id_is_a_warning_and_the_tree_still_updates` |
| The launcher's test harness stacked its OWN two layers, called `kernel.update()` directly, never set `BOUGH_HOME` and never touched `bough::compose` or `watch.rs`. The phase's exit gate therefore did not exercise the normative §0.5 layer stack. | `crates/bough/tests/support/mod.rs` lays out a real `$BOUGH_HOME` with `profiles/` and `bundles/` and boots through `bough::compose::compose_plan`; `recompose` calls `bough::watch::recompose_once`. It reproduces nothing. | the whole of `swap.rs`, `bad_patch.rs`, `invariants.rs` |
| V7's broadcast half was SELF-FULFILLING: the harness emitted `ConfigUpdateFailed` itself and then counted it. | The harness emits nothing. The broadcast comes from `recompose_once` (compose failure) or from inside the kernel (mount failure), and the compose payload is shared with the returned error rather than degraded to a string. | `crates/bough/tests/bad_patch.rs::invalid_config_leaves_last_good_tree_and_broadcasts_failure` · `kernel::tests::a_rejected_reconfigure_touches_nothing` |
| `swap.rs`'s closing "nothing owned by the retired fiber runs any more" re-composed the SAME patch: an empty diff, no dispatch, so the assertion held with the echo fiber fully alive. | The second recompose makes a real change; the test first asserts that the LIVE provider contributed a line, then that the retired one did not. | `crates/bough/tests/swap.rs::swapped_out_provider_leaves_no_listeners_and_no_bindings` |
| "Teardown before exit" was asserted by the exit code alone — it would have held with `kernel.shutdown()` deleted. | `hello` grew an `unload_marker` config field whose inverse touches a file; the boot tests assert on it across the process boundary, on both the SIGINT path and the failed-boot path. | `crates/bough/tests/boot.rs::{sigint_tears_down_before_exit, a_failed_boot_unwinds_the_rows_that_did_activate}` |
| The one runtime invariant the phase ships was falsified by the phase's own headline behaviour: a RELOAD keeps the `FiberUid` and restarts the seq at 1, against a process-global high-water mark. | `hello::apply` registers an inverse that forgets that fiber's recorded stream; the invariant is now stated per LIFE of a fiber. | `bough_plugin_hello::invariant::tests::forgetting_a_fiber_lets_a_reload_start_over` |
| `Context::fiber()` and `Context::kernel()` panicked. A `Context` clone outlives its fiber by construction, so a listener firing after unload panicked inside the kernel. | Both return `Option`; `ctx.mount()` returns `KernelError::Detached`. | (type-level) |
| `KernelOptions::reconcile_debounce` was written by every call site and read by nothing; the reconciler does not coalesce. | The field and the launcher's `RECONCILE_DEBOUNCE` constant are gone. | (removed) |
| `EffectHandle::dispose_detached` — "kills issued but not awaited", the shape §0.2 names as a bug — was public plugin-facing API. | `pub(crate)`, with the one legitimate use (a registration arriving after its fiber unwound) documented. | (visibility) |
| `Kernel::composition()` panicked on public API. | Returns `Option<Arc<Composition>>`. | (type-level) |
| `ScopeGuard` documented a `Drop` it does not have; `is_disabled` claimed an unevaluated `!!expr` "fails loud" (it does not); `event.rs` claimed a listener `Err` is caught (no listener signature can return one). | All three comments now say what the code does. | (documentation) |

### Open items, carried into later phases

1. **The lifecycle is a poll loop, not an event loop.** `drive` re-arms a 20ms timeout every idle
   pass, `FiberHandle::settled` polls at 20ms, `teardown` at 5ms, `quiesce_runtime` at 1ms; a
   PENDING fiber runs a full `resolve_view` store walk every tick. Tokio's `Notified` is armed from
   creation, so the timeouts are not needed for correctness — they are permanent CPU churn and they
   would mask a real missed-wakeup bug. Not changed here: it is a rewrite of the driver, and the
   phase's ordering guarantees are exactly what such a rewrite would put at risk. Phase 2, when a
   resident agent makes the cost visible.
2. **`quiesce()` is a stability heuristic**, not an edge-triggered barrier: three consecutive clean
   1ms passes over `settled_now()` fiber-by-fiber, with no snapshot, under a 10s ceiling. It now
   reports its own failure, but a fiber made unsettled by `poke_all()` between two passes can still
   be missed. Same fix as item 1.
3. **Ceilings remain on three waits**: `dispose` (5s), the UNLOADING dependent wait (5s),
   `quiesce_runtime` (10s). Each is now loud, and quiesce's is a returned `false`. They exist so a
   wedged plugin cannot hold the process open; removing them needs the plugin-side deadline story
   that does not exist until there are real plugins.
4. **Nested mounts are not accumulator entries.** `Kernel::mount_child` records the child in
   `Fiber::children`, and `teardown` disposes all children as one block before the accumulator, so
   `e1, mount(child), e2` unwinds `child, e2, e1` rather than the LIFO `e2, child, e1` §0.3 implies.
   The cascade itself is correct and tested; only the interleaving is wrong. Fixing it means making
   a mount a real `EffectHandle`, which is Phase 7 work (`wards-rhai`, `hooks-exec` and
   `mcp-subprocess` are the crates that graft child entries in anger).
5. **`Cadence::Interval` and `Cadence::OnEvent` are still not dispatched.** They are now loud
   (above), not implemented. Phase 1, with the ledger's own over-time invariant.
6. **`jsonschema` is declared in `[workspace.dependencies]` and used by no crate**, so
   `ErasedPlugin::schema()` has no callers and `ConfigError::Schema` is never constructed: config
   validation is serde deserialization plus `Plugin::validate`. §13 names the crate for "plugin
   `Config` and seal schemas; compiled validators", which is Phase 2 (worker seals). The pin is
   kept so the choice is not relitigated; the surface is dead until then.
7. **`emit` dispatch is spawned and never awaited.** `emit_ev` snapshots the admitted listeners,
   spawns, and drops the `JoinHandle`; nothing awaits outstanding emits at `Kernel::shutdown`, and
   a listener whose fiber unloads between emit and dispatch still runs. This is why every test that
   observes `FiberStateChanged` / `ConfigUpdated` / `ConfigUpdateFailed` polls or quiesces first.
   Contained (a panic cannot escape), but not ordered.
8. **The self-provision allowance in `check_declared` is name-keyed**, not realm- or scope-keyed:
   a fiber providing `X` in realm `alpha` may read `X` from the default realm without declaring it.
   A documented widening of §0.3's "a plugin reaches only what it declared". It is now answered from
   a recorded set rather than the live store, so the teardown bug is gone, but the widening stands.
9. **The unwound-accumulator tombstone has no dedicated test.** The leak it closes (an effect
   registered from inside an inverse) needs a fixture that misbehaves on purpose; the existing LIFO
   and cascade suites cover the normal paths only.
10. **`bough-base` ships TWO rows, not the one §17 asks for** (`greeting.provider` +
    `hello.greeter`). Forced by §0.2: a lone consumer row could never activate, and an enabled row
    that never activates is a boot failure. Stated in the bundle's own header; now also in
    `BUILD.md`.
11. **The profile layer is an inline `patch:` block inside `profiles/<name>.yml`**, not a
    per-profile `bough.patch.yml` file as §0.5 spells it. The layer ORDER matches and is tested
    (`compose::tests::layer_order_matches_requirements`); none of the three shipped profiles
    carries a `patch:` block, so the spelling itself is exercised only by unit tests.
12. **Decision D18 (a row may omit `plugin:` and be a pure group) is not implemented.** It is now
    rejected consistently by both the composer and the mount path rather than accepted by one and
    refused by the other.
