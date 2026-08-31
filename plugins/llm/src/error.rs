//! Invariant: the only errors this seam RETURNS are about composition (which adapter), never
//! about a model round; a round's failure is a terminal chunk (§12).

use crate::ids::AdapterName;

/// A failure of adapter selection.
#[derive(Debug, thiserror::Error)]
pub enum LlmSeamError {
    #[error("no adapter matches model `{model}`; registered: {registered:?}")]
    NoAdapter {
        model: String,
        registered: Vec<String>,
    },
    #[error("model `{model}` is matched equally by adapters `{a}` and `{b}`")]
    AmbiguousAdapter {
        model: String,
        a: AdapterName,
        b: AdapterName,
    },
}
