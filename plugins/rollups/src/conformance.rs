//! Invariant: BOTH providers are judged by ONE statement of the contract (the `plugins/ledger`
//! precedent). The suite is parameterised by `seals`, so the stub is held to "seals nothing,
//! refuses honestly, appends no step" and the summarizer to "seals once and indexes what it
//! sealed" — never to two different specs written twice.

use bough_plugin_ledger::LedgerHandle;

use crate::RollupsHandle;

/// The provider-conformance suite.
pub struct Conformance {
    /// `true` for a provider that actually seals; `false` for the truthful stub.
    pub seals: bool,
}

impl Conformance {
    /// Run every case against a mounted provider. `Err(case_name)` names the behaviour that broke.
    pub async fn run(&self, _handle: &RollupsHandle, _ledger: &LedgerHandle) -> Result<(), String> {
        todo!("WP-1: provider conformance suite")
    }
}
