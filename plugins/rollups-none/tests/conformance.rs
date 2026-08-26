//! The seam's provider-conformance suite, run against the STUB with `seals: false` (P4-D1). Both
//! providers are judged by ONE statement of the contract; this target is the stub's half of it.

use std::sync::Arc;

use bough_kernel::{Context, KernelCore};
use bough_plugin_ledger::LedgerHandle;
use bough_plugin_ledger_memory::store::MemoryStore;
use bough_plugin_rollups::{conformance::Conformance, RollupsHandle};
use bough_plugin_rollups_none::NoneSummarizer;

#[tokio::test]
async fn the_stub_passes_the_conformance_suite_as_a_non_sealing_provider() {
    let ctx = Context::root(KernelCore::new());
    let ledger = LedgerHandle(MemoryStore::new(ctx) as Arc<_>);
    let handle = RollupsHandle(Arc::new(NoneSummarizer {
        ledger: Arc::new(ledger.clone()),
    }));
    Conformance { seals: false }
        .run(&handle, &ledger)
        .await
        .unwrap_or_else(|case| panic!("the stub failed conformance case `{case}`"));
}
