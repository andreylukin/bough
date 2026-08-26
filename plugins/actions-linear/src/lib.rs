//! Invariant: `linear_write` can change a ticket's STATUS or leave a COMMENT, and nothing else.
//! EXACTLY ONE of [`LinearWritePayload`]'s two fields is `Some`; a payload naming a title, a team
//! or a new issue is refused. That refusal exists in ADDITION to the absent `create_ticket` kind,
//! so "ticket creation stays Andrey's" is enforced twice and by different mechanisms.
//!
//! The API key is redacted from every rendering, exactly as in `collector-linear` (P6-D7).

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_actions::{
    ActionArtifact, ActionError, ActionKind, ActionProvider, ExecuteRequest,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "actions-linear";

/// The row's config.
#[derive(Clone, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearActionsConfig {
    pub endpoint: String,
    /// `!!expr 'env("LINEAR_API_KEY")'`. Redacted everywhere (P6-D7).
    pub api_key: String,
    pub timeout_ms: u64,
}

impl std::fmt::Debug for LinearActionsConfig {
    /// WP-3: every field, with `api_key` rendered as `<redacted>`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let _ = f;
        todo!("WP-3")
    }
}

/// `linear_write`'s payload. EXACTLY ONE of the two is `Some`.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LinearWritePayload {
    pub status: Option<String>,
    pub comment: Option<String>,
}

impl LinearWritePayload {
    /// PURE: refuse a payload that is neither, both, or that names anything creation-shaped.
    /// `deny_unknown_fields` already refuses a `title`/`team`; this is the "exactly one" half. WP-3.
    pub fn check(&self) -> Result<(), LinearActionError> {
        todo!("WP-3")
    }
}

/// The Provider.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct LinearActions {
    cfg: Arc<LinearActionsConfig>,
    http: reqwest::Client,
}

impl LinearActions {
    /// Build the Provider. WP-3.
    pub fn open(cfg: Arc<LinearActionsConfig>) -> Result<Arc<LinearActions>, LinearActionError> {
        let _ = cfg;
        todo!("WP-3")
    }
}

#[async_trait::async_trait]
impl ActionProvider for LinearActions {
    fn kinds(&self) -> Vec<ActionKind> {
        vec![ActionKind::LinearWrite]
    }

    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        let _ = req;
        todo!("WP-3: check the payload, then ONE mutation; the comment carries `req.marker` as a suffix")
    }
}

/// What this Provider refuses.
///
/// `plugins/actions` is off-limits in this track and [`ActionError`] has no `BadPayload` variant,
/// so a refusal surfaces as `ActionError::Provider { kind, source }` wrapping one of these. Merge
/// note: `ActionError::BadPayload`.
#[derive(Debug, thiserror::Error)]
pub enum LinearActionError {
    #[error("linear_write refused: exactly one of `status` or `comment` must be set ({detail})")]
    BadPayload { detail: String },
    #[error("linear_write refused: creating tickets is Andrey's, not the harness's (`{field}`)")]
    Creation { field: &'static str },
    #[error("transport: {0}")]
    Transport(String),
    #[error("linear: {0}")]
    Server(String),
}

/// The row.
pub struct LinearActionsPlugin;

#[async_trait::async_trait]
impl Plugin for LinearActionsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LinearActionsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["actions"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-3: a parseable `endpoint`, `timeout_ms > 0`")
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-3: register on ctx.actions + the ArtifactLookup registry, both as effects")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(LinearActionsPlugin);
