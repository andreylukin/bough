//! Invariant (§10): a worker's report is SEALED. It is validated against a compiled schema, and a
//! report that does not validate is a worker FAILURE — never a silently-accepted blob that the
//! spawner then cites as evidence.

use std::sync::{Arc, OnceLock};

use bough_plugin_ledger::{Cite, Ref};

use crate::ids::WorkerId;

/// One compiled seal.
///
/// The validator is compiled ONCE, lazily, and shared by every clone: a seal is handed to every
/// worker start, and compiling per validation would pay a schema compile for each report.
#[derive(Clone)]
pub struct SealSpec {
    pub name: String,
    pub schema: Arc<schemars::Schema>,
    compiled: Arc<OnceLock<Result<Arc<jsonschema::Validator>, String>>>,
}

impl std::fmt::Debug for SealSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SealSpec")
            .field("name", &self.name)
            .finish()
    }
}

impl SealSpec {
    /// A seal over any `JsonSchema` type.
    pub fn of<T: schemars::JsonSchema>(name: &str) -> SealSpec {
        SealSpec::new(
            name,
            schemars::SchemaGenerator::default().into_root_schema_for::<T>(),
        )
    }

    /// A seal over a schema that is already in hand.
    pub fn new(name: &str, schema: schemars::Schema) -> SealSpec {
        SealSpec {
            name: name.to_string(),
            schema: Arc::new(schema),
            compiled: Arc::new(OnceLock::new()),
        }
    }

    /// The built-in `worker.report` seal.
    pub fn report() -> SealSpec {
        SealSpec::of::<Report>("worker.report")
    }

    /// Validate a report body against this seal, compiled once with `jsonschema`.
    ///
    /// `Err` carries the first failing instance pointer, so a spawner can tell the model WHICH
    /// field of the report was wrong instead of "invalid".
    pub fn validate(&self, body: &serde_json::Value) -> Result<(), String> {
        let validator = self
            .compiled
            .get_or_init(|| {
                jsonschema::validator_for(self.schema.as_value())
                    .map(Arc::new)
                    .map_err(|e| format!("uncompilable seal schema: {e}"))
            })
            .as_ref()
            .map_err(|e| e.clone())?;
        // `iter_errors` so the message names the first failing POINTER; `validate`'s own error
        // says only that something failed.
        match validator.iter_errors(body).next() {
            None => Ok(()),
            Some(e) => {
                let at = e.instance_path.to_string();
                let at = if at.is_empty() { "/".to_string() } else { at };
                Err(format!("at `{at}`: {e}"))
            }
        }
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
    #[serde(default)]
    pub cites: Vec<Cite>,
}

/// The ref a worker's own report is cited by. The one spelling; both sides of the
/// evidence/thought predicate use it.
pub fn worker_ref(worker: &WorkerId) -> Ref {
    Ref::new(format!("worker:{worker}"))
}

impl ReportClaim {
    /// Whether this claim cites anything OUTSIDE the worker's own report. The predicate that
    /// decides `worker/report` evidence from `worker/claim` thought (§10).
    pub fn is_externally_cited(&self, worker: &WorkerId) -> bool {
        let own = worker_ref(worker);
        self.cites.iter().any(|c| c.r#ref != own)
    }

    /// The claim's external cites alone.
    pub fn external_cites(&self, worker: &WorkerId) -> Vec<Cite> {
        let own = worker_ref(worker);
        self.cites
            .iter()
            .filter(|c| c.r#ref != own)
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cite(r: &str) -> Cite {
        Cite {
            r#ref: Ref::new(r),
            url: None,
        }
    }

    #[test]
    fn the_builtin_seal_accepts_a_well_formed_report() {
        let seal = SealSpec::report();
        assert_eq!(seal.name, "worker.report");
        let body = serde_json::json!({
            "summary": "did the thing",
            "claims": [{ "text": "line 3 changed", "cites": [{ "ref": "step:abc" }] }]
        });
        seal.validate(&body)
            .expect("a well-formed report validates");
    }

    /// The failure NAMES the pointer, because "invalid" is not something a spawner can act on.
    #[test]
    fn a_report_missing_summary_is_refused_at_a_named_pointer() {
        let seal = SealSpec::report();
        let err = seal
            .validate(&serde_json::json!({ "claims": [] }))
            .expect_err("a report without a summary is not a report");
        assert!(err.contains("summary"), "unhelpful refusal: {err}");
    }

    #[test]
    fn a_claim_of_the_wrong_shape_is_refused() {
        let seal = SealSpec::report();
        let err = seal
            .validate(&serde_json::json!({ "summary": "s", "claims": [{ "text": 7 }] }))
            .expect_err("a numeric claim text is not a claim");
        assert!(err.contains("claims"), "wrong pointer: {err}");
    }

    /// §10's predicate: only a cite OUTSIDE the worker's own report makes a claim evidence.
    #[test]
    fn a_claim_citing_only_its_own_report_is_not_externally_cited() {
        let w = WorkerId::new("w1");
        let own = ReportClaim {
            text: "trust me".into(),
            cites: vec![cite("worker:w1")],
        };
        let bare = ReportClaim {
            text: "trust me".into(),
            cites: vec![],
        };
        let external = ReportClaim {
            text: "line 3 changed".into(),
            cites: vec![cite("worker:w1"), cite("step:abc")],
        };
        assert!(!own.is_externally_cited(&w));
        assert!(!bare.is_externally_cited(&w));
        assert!(external.is_externally_cited(&w));
        assert_eq!(external.external_cites(&w), vec![cite("step:abc")]);
    }
}
