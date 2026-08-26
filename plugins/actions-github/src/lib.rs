//! Invariant: this Provider writes to GitHub ONLY through `gh_cli::Gh::run`, and only after a
//! pre-flight LOOKUP has proved the act is inside §7's boundary:
//!
//! - `push_to_pr` only onto a PR **Andrey authored** and that is **open** — never a teammate's
//!   branch. The author comparison is against `gh api user`'s login, cached per activation.
//! - `bot_thread_op` only on a thread whose opener classifies as [`Actor::Bot`]. **Uncertain is
//!   human**, and a human thread is never auto-resolved.
//!
//! And every artifact CARRIES THE MARKER derived from the idem key, so reconciliation is a lookup
//! and never a guess (§7): PR body last line, commit trailer, comment suffix.
//!
//! [`Actor::Bot`]: bough_plugin_gh_cli::Actor::Bot

pub mod invariant;
pub mod marker;

use std::sync::Arc;

use bough_kernel::{ConfigError, Context, InvariantSpec, Plugin, PluginError};
use bough_plugin_actions::{
    ActionArtifact, ActionError, ActionKind, ActionProvider, ExecuteRequest,
};
use bough_plugin_gh_cli::Gh;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "actions-github";

/// The row's config.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct GithubActionsConfig {
    /// `"gh"`. The tests put a recording shim here.
    pub gh_bin: String,
    /// The known-bot allowlist `gh_cli::classify` consults.
    pub known_bots: Vec<String>,
    pub timeout_ms: u64,
}

/// `open_pr`'s payload.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenPrPayload {
    pub head: String,
    pub base: String,
    pub title: String,
    pub body: String,
}

/// `push_to_pr`'s payload.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PushToPrPayload {
    pub branch: String,
    pub commits: Vec<String>,
}

/// `bot_thread_op`'s payload.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BotThreadPayload {
    pub thread: String,
    pub op: ThreadOp,
    pub body: Option<String>,
}

/// What may be done to a BOT review thread. There is no `create` and no human variant.
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ThreadOp {
    Reply,
    Resolve,
    Close,
}

/// The Provider.
#[allow(dead_code)] // scaffold: filled by the work package that owns this crate
pub struct GithubActions {
    cfg: Arc<GithubActionsConfig>,
    gh: Gh,
    /// `gh api user`'s login, resolved ONCE at activation. The author comparison reads it.
    me: parking_lot::Mutex<Option<String>>,
}

impl GithubActions {
    /// Build the Provider and resolve `me`. WP-3.
    pub async fn open(cfg: Arc<GithubActionsConfig>) -> Result<Arc<GithubActions>, GhActionError> {
        let _ = cfg;
        todo!("WP-3")
    }

    /// PRE-FLIGHT: `gh pr view --json author,state,isDraft,headRefName`, compared to `me`. WP-3.
    pub async fn check_push_target(&self, target: &str) -> Result<(), GhActionError> {
        let _ = target;
        todo!("WP-3: NotAuthored / NotOpen, before anything is written")
    }

    /// PRE-FLIGHT: the thread's first comment's `user.type` / `user.login` through
    /// `gh_cli::classify`. [`Actor::Human`] refuses. WP-3.
    ///
    /// [`Actor::Human`]: bough_plugin_gh_cli::Actor::Human
    pub async fn check_bot_thread(&self, thread: &str) -> Result<(), GhActionError> {
        let _ = thread;
        todo!("WP-3: NotABot, quoting `classify_reason` so `uncertain` is visible")
    }
}

#[async_trait::async_trait]
impl ActionProvider for GithubActions {
    fn kinds(&self) -> Vec<ActionKind> {
        vec![
            ActionKind::OpenPr,
            ActionKind::PushToPr,
            ActionKind::BotThreadOp,
        ]
    }

    async fn execute(&self, req: &ExecuteRequest) -> Result<ActionArtifact, ActionError> {
        let _ = req;
        todo!("WP-3: pre-flight, embed `req.marker`, one `gh` write, return the artifact's locator")
    }
}

/// Pre-flight refusals, each a lookup against the world before anything is written.
#[derive(Debug, thiserror::Error)]
pub enum GhActionError {
    #[error("push_to_pr refused: {target} is authored by `{author}`, not `{me}` (§7: never teammates' branches)")]
    NotAuthored {
        target: String,
        author: String,
        me: String,
    },
    #[error("push_to_pr refused: {target} is {state}, not open")]
    NotOpen { target: String, state: String },
    #[error("bot_thread_op refused: {thread} was opened by `{login}` ({reason}); human threads are never auto-resolved")]
    NotABot {
        thread: String,
        login: String,
        reason: &'static str,
    },
    #[error("payload for `{kind}` is not what §7 sanctions: {detail}")]
    BadPayload { kind: &'static str, detail: String },
    #[error(transparent)]
    Gh(#[from] bough_plugin_gh_cli::GhError),
}

/// The row.
pub struct GithubActionsPlugin;

#[async_trait::async_trait]
impl Plugin for GithubActionsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = GithubActionsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["actions"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), ConfigError> {
        let _ = cfg;
        todo!("WP-3: non-empty `gh_bin`, `timeout_ms > 0`")
    }

    /// Register the Provider on `ctx.actions` as an effect, and its [`ArtifactLookup`] half on
    /// `actions-reconcile`'s registry. WP-3.
    ///
    /// [`ArtifactLookup`]: bough_plugin_actions_reconcile::ArtifactLookup
    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-3")
    }

    fn invariants() -> Vec<InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(GithubActionsPlugin);
