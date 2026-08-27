//! Invariant: the kernel is domain-blind (§0.1 item 1). It knows contexts, typed service keys,
//! fibers, effects, events, scopes, config trees and patch layers — and nothing about agents,
//! ledgers, steps, models or terminals. A domain noun appearing in this crate is a failed review.

// Re-exported so `register_plugin!` works in a crate that does not name `inventory` itself.
#[doc(hidden)]
pub use inventory;

pub mod catalog;
pub mod config;
pub mod context;
pub mod effect;
pub mod error;
pub mod event;
pub mod fiber;
pub mod invariant;
pub mod kernel;
pub mod plugin;
pub mod reconcile;
pub mod scope;
pub mod service;

pub use catalog::{Catalog, PluginRegistration};
pub use config::{
    render, ComposeError, ComposeWarning, Composer, Composition, DumpFormat, Entry, EntryPatch,
    Expr, ExprEnv, Fingerprint, Inject, LayerId, Patch, RealmLabel, RowProvenance,
};
pub use context::{default_realm, CommittedView, Context, KernelCore};
pub use effect::{EffectCtx, EffectHandle, Halted};
pub use error::{ConfigError, KernelError, PluginError};
pub use event::{
    ConfigReload, ConfigReloadEvent, DispatchMode, EmitEvent, ListenerOpts, Next, ParallelEvent,
    SerialEvent, WaterfallEvent,
};
pub use fiber::{EntryId, FiberHandle, FiberState, FiberUid};
pub use invariant::{Cadence, InvariantSpec, InvariantViolation};
pub use kernel::{Kernel, KernelOptions, RowSnapshot, TreeSnapshot, UnresolvedRow};
pub use plugin::{ErasedConfig, ErasedPlugin, Plugin, Reconfigure};
pub use scope::{create_scope, scope_target, ScopeGuard, ScopeKey, ScopedDispatch};
pub use service::{Binding, ProviderUid, ServiceKey, ServiceSlot};
