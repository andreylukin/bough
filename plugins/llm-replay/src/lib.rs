//! Invariant: this crate is the OFFLINE `llm` provider. Everything the hermetic suite runs
//! against goes through it (AGENTS.md: the default suite never touches the network), and its
//! answers are a pure function of the transcript and the request.

pub mod invariant;
pub mod transcript;

use std::path::PathBuf;
use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_llm::{AdapterName, LlmAdapter, LlmRequest, LlmStream};
use tokio_util::sync::CancellationToken;

pub use transcript::{RecordedChunk, Round, Transcript};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "llm-replay";

/// The row's config: a file, or the rounds inline.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplayConfig {
    #[serde(default)]
    pub transcript: Option<PathBuf>,
    /// Rounds written straight into the patch, for a test that wants no fixture file. Raw JSON,
    /// parsed by [`Transcript::parse`] — the config schema stays shallow on purpose.
    #[serde(default)]
    pub rounds: Option<serde_json::Value>,
    /// `true` (the default): an unmatched request is `Chunk::Failed { BadRequest }`.
    #[serde(default = "yes")]
    pub strict: bool,
    /// Which models this adapter claims. `"*"` by default, so a swap patch needs one line.
    #[serde(default = "star")]
    pub models: String,
}

fn yes() -> bool {
    true
}
fn star() -> String {
    "*".to_string()
}

/// The replaying adapter.
pub struct ReplayAdapter {
    _cfg: Arc<ReplayConfig>,
}

impl ReplayAdapter {
    /// WP-1.
    pub fn new(cfg: Arc<ReplayConfig>) -> ReplayAdapter {
        ReplayAdapter { _cfg: cfg }
    }
}

#[async_trait::async_trait]
impl LlmAdapter for ReplayAdapter {
    fn name(&self) -> AdapterName {
        AdapterName::new(PLUGIN_NAME)
    }

    /// WP-1.
    async fn start(&self, _req: Arc<LlmRequest>, _cancel: CancellationToken) -> LlmStream {
        todo!("WP-1: select the round and yield its chunks, terminal chunk last")
    }
}

/// The provider row. In the catalog, in NO bundle: the swap patches name it (§17 Phase 2).
pub struct ReplayPlugin;

#[async_trait::async_trait]
impl Plugin for ReplayPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = ReplayConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["llm"])
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        match (&cfg.transcript, &cfg.rounds) {
            (None, None) => Err(bough_kernel::ConfigError::Rejected {
                detail: "llm-replay needs either `transcript:` or `rounds:`".to_string(),
            }),
            (Some(_), Some(_)) => Err(bough_kernel::ConfigError::Rejected {
                detail: "llm-replay takes `transcript:` or `rounds:`, not both".to_string(),
            }),
            _ => Ok(()),
        }
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: load the transcript and register the adapter as an effect")
    }
}

bough_kernel::register_plugin!(ReplayPlugin);
