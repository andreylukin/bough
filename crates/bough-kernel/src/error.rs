//! Invariant: every kernel error names the row and, where the fault is a plugin's, the plugin.
//! An error a human cannot act on without reading the source is a bug in this module (§0.2).

use crate::config::{ExprError, LayerId};
use crate::fiber::EntryId;

/// Failures raised by the kernel handle and by `Context`.
#[derive(Debug, thiserror::Error)]
pub enum KernelError {
    /// The §0.3 capability check, reported at the point of use — even when the key happens to be
    /// bound. The message is normative; see §2.11 of the Phase 0 plan.
    #[error(
        "plugin `{plugin}` (row `{entry}`) read service `{key}` without declaring it in inject"
    )]
    UndeclaredService {
        plugin: &'static str,
        entry: EntryId,
        key: &'static str,
    },
    /// Declared optional, and no active fiber provides it.
    #[error("plugin `{plugin}` (row `{entry}`) read optional service `{key}`, which no active fiber provides")]
    ServiceUnavailable {
        plugin: &'static str,
        entry: EntryId,
        key: &'static str,
    },
    #[error("row `{0}` is not in the tree")]
    NoSuchRow(EntryId),
    #[error("duplicate row id `{0}`")]
    DuplicateRowId(EntryId),
    #[error(transparent)]
    Compose(#[from] ComposeError),
}

/// Failures composing a candidate tree. A `ComposeError` never disturbs the running tree: the last
/// good tree keeps running and `config-update-failed` is broadcast (§0.3).
#[derive(Debug, thiserror::Error)]
pub enum ComposeError {
    #[error("row `{entry}` names plugin `{plugin}`, which is not in the catalog")]
    UnknownPlugin {
        entry: EntryId,
        plugin: String,
        layer: LayerId,
    },
    #[error("row `{entry}` (plugin `{plugin}`): {source}")]
    BadConfig {
        entry: EntryId,
        plugin: String,
        layer: LayerId,
        #[source]
        source: ConfigError,
    },
    #[error("layer `{layer}`: {source}")]
    BadExpr {
        layer: LayerId,
        #[source]
        source: ExprError,
    },
    #[error("layer `{layer}`: {detail}")]
    BadYaml { layer: LayerId, detail: String },
    #[error("include `{path}` (from layer `{layer}`): {detail}")]
    BadInclude {
        path: std::path::PathBuf,
        layer: LayerId,
        detail: String,
    },
}

/// A plugin `Config` that failed schema validation, deserialization, or `Plugin::validate`.
/// Validation is pure and synchronous (§0.5): a check needing I/O belongs in `apply`.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("config does not match the plugin schema: {detail}")]
    Schema { detail: String },
    #[error("config could not be deserialized: {detail}")]
    Deserialize { detail: String },
    #[error("config rejected by the plugin: {detail}")]
    Rejected { detail: String },
}

/// A failure raised by plugin code — from `apply`, from an effect body, or from a listener.
/// anyhow-backed so a plugin can attach whatever context it likes; the kernel attaches the row.
#[derive(Debug, thiserror::Error)]
#[error("row `{entry}`: {source}")]
pub struct PluginError {
    pub entry: EntryId,
    #[source]
    pub source: anyhow::Error,
}

impl PluginError {
    /// Attach a row id to an arbitrary error.
    pub fn new(entry: EntryId, source: impl Into<anyhow::Error>) -> Self {
        Self {
            entry,
            source: source.into(),
        }
    }
}
