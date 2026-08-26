//! Invariant: ONE writer. Every store call runs inside `tokio::task::spawn_blocking` over one
//! `Arc<Mutex<Connection>>` — that mutex IS §3's single writer — and `seq` is allocated by
//! `MAX(seq)+1` INSIDE the insert transaction, so two concurrent appends can neither collide nor
//! gap (P1-D9, P1-D15).

use std::sync::Arc;

use bough_kernel::Context;
use bough_plugin_ledger::{LedgerError, StepTypeMap};
use parking_lot::Mutex;
use rusqlite::Connection;

use crate::SqliteConfig;

/// The store behind the `ledger` binding.
pub struct SqliteStore {
    #[doc(hidden)]
    pub(crate) conn: Arc<Mutex<Connection>>,
    /// The merge-extensible step-type map, preloaded with the sixteen builtins.
    pub(crate) types: Arc<StepTypeMap>,
    /// The provider's captured context: `ledger/step` is emitted from it, post-commit.
    pub(crate) ctx: Context,
    /// Rows skipped on read because their type was unknown AND ignorable.
    pub(crate) skipped: Arc<std::sync::atomic::AtomicU64>,
}

impl SqliteStore {
    /// Open (or create) the db, check the format version, install the schema and the builtins.
    pub fn open(cfg: &SqliteConfig, ctx: Context) -> Result<Arc<SqliteStore>, LedgerError> {
        todo!("WP-2: SqliteStore::open")
    }

    /// Run `f` against the single connection on a blocking thread.
    pub(crate) async fn with_conn<T, F>(&self, f: F) -> Result<T, LedgerError>
    where
        T: Send + 'static,
        F: FnOnce(&mut Connection) -> Result<T, LedgerError> + Send + 'static,
    {
        todo!("WP-2: SqliteStore::with_conn")
    }
}
