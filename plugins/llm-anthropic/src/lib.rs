//! Invariant: this crate is a PROVIDER of the `llm` seam and nothing else. It wraps
//! `bough_llm::client_for` with retries DISABLED (P2-D5: retry is `llm-retry`'s waterfall
//! listener, and two retry layers would make the attempt counter a lie), and it never throws:
//! an absent key, a transport error and a refusal all leave as `Chunk::Failed` (P2-D7).

pub mod invariant;
pub mod map;

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_plugin_llm::{AdapterName, LlmAdapter, LlmRequest, LlmStream};
use tokio_util::sync::CancellationToken;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "llm-anthropic";

/// The row's config. `models` is a [`bough_plugin_llm::ModelMatch`] spelling.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AnthropicConfig {
    /// Which models this adapter claims: `"*"`, `"claude-*"`, or an exact id.
    #[serde(default = "default_models")]
    pub models: String,
    /// The environment variable the API key is read from, at CALL time.
    #[serde(default = "default_key_env")]
    pub api_key_env: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default = "default_timeout")]
    pub request_timeout_ms: u64,
}

fn default_models() -> String {
    "*".to_string()
}
fn default_key_env() -> String {
    "ANTHROPIC_API_KEY".to_string()
}
fn default_timeout() -> u64 {
    120_000
}

/// The adapter this row registers.
pub struct AnthropicAdapter {
    _cfg: Arc<AnthropicConfig>,
}

impl AnthropicAdapter {
    /// WP-1.
    pub fn new(cfg: Arc<AnthropicConfig>) -> AnthropicAdapter {
        AnthropicAdapter { _cfg: cfg }
    }
}

#[async_trait::async_trait]
impl LlmAdapter for AnthropicAdapter {
    fn name(&self) -> AdapterName {
        AdapterName::new(PLUGIN_NAME)
    }

    /// WP-1: `client_for(model, ClientOpts { retry: RetryOpts::none(), .. })`, `run` + `on_text`,
    /// mapped through [`map`]. Never `Err`.
    async fn start(&self, _req: Arc<LlmRequest>, _cancel: CancellationToken) -> LlmStream {
        todo!("WP-1: drive bough-llm and map the round onto the chunk vocabulary")
    }
}

/// The provider row.
pub struct AnthropicPlugin;

#[async_trait::async_trait]
impl Plugin for AnthropicPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = AnthropicConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["llm"])
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: llm.adapter(ctx, an AdapterSpec) — registration is an effect")
    }
}

bough_kernel::register_plugin!(AnthropicPlugin);
