//! The provider-conformance suite (`bough_plugin_ledger::conformance`), expanded here as named
//! tests. Its whole point is that `ledger-sqlite` and `ledger-memory` answer identically: a
//! divergence shows up as the SAME test name failing on one provider.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::conformance::{EventTap, Fixture};
use bough_plugin_ledger::{LedgerHandle, LedgerStep};
use bough_plugin_ledger_sqlite::{store::SqliteStore, SqliteConfig};

/// One fixture per case: a fresh in-memory ledger, its context, and a tap on `ledger/step`.
async fn fixture() -> Fixture {
    let ctx = Context::root(KernelCore::new());
    let store = SqliteStore::open(
        &SqliteConfig {
            path: ":memory:".into(),
            busy_timeout_ms: 5000,
        },
        ctx.clone(),
    )
    .expect("in-memory ledger opens");

    let tap = EventTap::default();
    let seen = tap.seen.clone();
    ctx.on::<LedgerStep, _, _>(move |step| {
        let seen = seen.clone();
        async move {
            seen.lock().push(step);
        }
    })
    .await
    .expect("the tap registers");

    Fixture {
        ledger: LedgerHandle(store as Arc<_>),
        ctx,
        tap,
    }
}

bough_plugin_ledger::ledger_conformance!(|| async { fixture().await });
