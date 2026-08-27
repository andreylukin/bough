//! Invariant: the surface the model READS and the surface it GETS are built from one snapshot.
//! Every injected global is a registered `ToolSpec` visible in the agent's scope, and nothing
//! else is injected.

use std::collections::BTreeMap;

use bough_plugin_js::{HostFn, RefusalKind};
use bough_plugin_tools::{FailureClass, ToolSpec};

/// A tool's registered name mapped onto the JS identifier the sandbox injects.
#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    /// The JS name, possibly dotted (`ledger.search`, `bg.output`).
    pub js: String,
    /// The registered `ToolName` the call resolves to.
    pub tool: String,
}

/// Why a name could not be injected.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum BindError {
    #[error("`{0}` is not a legal JS identifier path and cannot be injected")]
    NotAnIdentifier(String),
    #[error("`{js}` is claimed by both `{a}` and `{b}`")]
    Collision { js: String, a: String, b: String },
}

/// Turn the visible specs plus the row's aliases and namespaces into the binding list.
/// A dotted name builds a namespace object; a name that is both a function and a namespace root
/// becomes a callable object.
///
/// WP-2 owns the body.
pub fn bindings(
    _specs: &[ToolSpec],
    _aliases: &BTreeMap<String, String>,
    _namespaces: &BTreeMap<String, String>,
) -> Result<Vec<Binding>, BindError> {
    todo!("WP-2: name → JS identifier, aliases, namespace grouping, collision detection")
}

/// Build the `HostFn` for one binding. Each body mints the deterministic `{run}.{n}` call id,
/// appends `program/call`, runs the mirror's pipeline, appends `program/result`, and answers.
///
/// WP-2 owns the body.
pub fn host_fn(_b: &Binding, _spec: &ToolSpec) -> HostFn {
    todo!("WP-2: the HostCall body that drives one inner call through the mirror pipeline")
}

/// The one mapping between the tools seam's failure taxonomy and the sandbox's.
pub fn refusal_of(class: FailureClass) -> RefusalKind {
    match class {
        FailureClass::NotFound => RefusalKind::NotFound,
        FailureClass::Denied => RefusalKind::Denied,
        FailureClass::Blocked => RefusalKind::Blocked,
        FailureClass::Timeout => RefusalKind::Timeout,
        FailureClass::Cancelled => RefusalKind::Cancelled,
        // Crash repair's synthesised outcome: a program never sees it live, and if it ever did
        // it is an error like any other rather than a seventh kind in the sandbox.
        FailureClass::Unknown | FailureClass::Error => RefusalKind::Error,
    }
}
