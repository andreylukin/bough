//! Invariant: the kernel handle owns the running tree and is the only writer of it. A live
//! recompose that fails leaves the last good tree untouched and has already broadcast
//! `config-update-failed` (§0.3); `shutdown` unloads everything, LIFO, awaited, so a caller can
//! restore a terminal after it returns (§0.1 item 2, teardown-before-exit).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use crate::catalog::Catalog;
use crate::config::{Composition, Fingerprint, RealmLabel};
use crate::context::Context;
use crate::error::KernelError;
use crate::fiber::{EntryId, FiberState, FiberUid};
use crate::invariant::InvariantViolation;

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

/// The running tree.
pub struct Kernel {
    _priv: (),
}

impl Kernel {
    /// Build a kernel over a catalog. Nothing is mounted until [`Kernel::load`].
    pub fn new(catalog: Catalog, options: KernelOptions) -> Arc<Kernel> {
        todo!("WP-3")
    }
    /// The root context: the parent of every top-level row.
    pub fn root(&self) -> Context {
        todo!("WP-3")
    }
    /// Mount a composition for the first time.
    pub async fn load(&self, c: Composition) -> Result<(), KernelError> {
        todo!("WP-3")
    }
    /// Live recompose. On `Err` the last good tree is untouched and `config-update-failed` has
    /// already been emitted (§0.3).
    pub async fn update(&self, c: Composition) -> Result<(), KernelError> {
        todo!("WP-3")
    }
    /// Return once no fiber is Loading or Unloading and no reconcile is pending — including
    /// fibers that a transition itself created. The workhorse of every test.
    pub async fn quiesce(&self) {
        todo!("WP-3")
    }
    /// The structural view tests assert on.
    pub fn snapshot(&self) -> TreeSnapshot {
        todo!("WP-3")
    }
    /// The composition currently live.
    pub fn composition(&self) -> Arc<Composition> {
        todo!("WP-3")
    }
    /// Violations recorded by the invariant runner; empty when it is not running.
    pub fn violations(&self) -> Vec<InvariantViolation> {
        todo!("WP-3")
    }
    /// Unload everything, LIFO, awaited.
    pub async fn shutdown(&self) {
        todo!("WP-3")
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
        todo!("WP-3")
    }
}
