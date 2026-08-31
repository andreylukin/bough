//! The provider-conformance suite, expanded into NAMED tests (P1-D10). It is the same expansion
//! `ledger-sqlite` carries, so any divergence between the two providers shows up as the same named
//! test failing on one of them — and neither can quietly skip a case.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::conformance::{EventTap, Fixture};
use bough_plugin_ledger::{LedgerHandle, LedgerStep};
use bough_plugin_ledger_memory::store::MemoryStore;

/// A fresh store on a fresh kernel core, with a tap on `ledger/step` already registered — so a
/// case that observes the event awaits a receipt instead of sleeping.
async fn fixture() -> Fixture {
    let ctx = Context::root(KernelCore::new());
    let store = MemoryStore::new(ctx.clone());
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
