//! Invariant (§10): a worker's report is SEALED. It is validated against a compiled schema, and a
//! report that does not validate is a worker FAILURE — never a silently-accepted blob that the
//! spawner then cites as evidence.

use std::sync::Arc;

use bough_plugin_ledger::Cite;

use crate::ids::WorkerId;

/// One compiled seal.
#[derive(Clone)]
pub struct SealSpec {
    pub name: String,
    pub schema: Arc<schemars::Schema>,
}

impl SealSpec {
    /// The built-in `worker.report` seal. WP-6.
    pub fn report() -> SealSpec {
        todo!("WP-6: schemars schema for Report, named worker.report")
    }

    /// Validate a report body against this seal, compiled once with `jsonschema`. WP-6.
    pub fn validate(&self, _body: &serde_json::Value) -> Result<(), String> {
        todo!("WP-6: compile once, report the first failing pointer")
    }
}

/// What a worker reports back (§10).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct Report {
    pub summary: String,
    /// §10: per-claim EXTERNAL cites. A claim whose only citation is this report is a THOUGHT.
    pub claims: Vec<ReportClaim>,
}

/// One claim of a report.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ReportClaim {
    pub text: String,
    pub cites: Vec<Cite>,
}

impl ReportClaim {
    /// Whether this claim cites anything OUTSIDE the worker's own report. The predicate that
    /// decides `worker/report` evidence from `worker/claim` thought (§10).
    ///
    /// WP-6.
    pub fn is_externally_cited(&self, _worker: &WorkerId) -> bool {
        todo!("WP-6: a cite that is not this worker's own report step")
    }
}
