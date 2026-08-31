//! Invariant: this crate is a PROVIDER of the `llm` seam and nothing else — `llm-openai`'s twin
//! over `bough_llm`'s OpenRouter chat-completions client. It wraps `bough_llm::client_for` with
//! retries DISABLED (P2-D5: retry is `llm-retry`'s waterfall listener), maps rounds through the
//! seam's shared `adapt` module, and never throws: an absent key, a transport error and a refusal
//! all leave as `Chunk::Failed` (P2-D7).
//!
//! It claims `openrouter:*` and STRIPS that prefix before the client: `bough-llm`'s native
//! OpenRouter spelling is a bare `vendor/model` id (anything with a `/` routes there), but a bare
//! id cannot be claimed by a `ModelMatch` prefix without also claiming half the world — the row's
//! spelling is the policy's (`sol: openrouter:qwen/qwen-3`), the wire's is OpenRouter's own. A
//! stripped id with NO slash would silently route to Anthropic inside `client_for`, so it is
//! refused as a terminal `BadRequest` naming the required shape instead.

pub mod invariant;

use std::sync::Arc;

use bough_kernel::{Context, Plugin, PluginError};
use bough_llm::types::LlmClient;
use bough_plugin_llm::{
    adapt, AdapterName, AdapterSpec, Chunk, Llm, LlmAdapter, LlmRequest, LlmStream, ModelMatch,
};
use tokio_util::sync::CancellationToken;

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "llm-openrouter";

/// The row's spelling of an OpenRouter model id. Stripped before the wire.
pub const PREFIX: &str = "openrouter:";

/// The row's config. `models` is a [`bough_plugin_llm::ModelMatch`] spelling.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct OpenrouterConfig {
    /// Which models this adapter claims: `"openrouter:*"`, or an exact id.
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
    "openrouter:*".to_string()
}
fn default_key_env() -> String {
    "OPENROUTER_API_KEY".to_string()
}
fn default_timeout() -> u64 {
    120_000
}

/// The wire id for a policy spelling: the prefix stripped. `Err` names the shape when what is
/// left could not route to OpenRouter (`client_for` sends a slashless id to Anthropic — a silent
/// misroute, which is exactly what a loud refusal here prevents).
pub fn wire_model(model: &str) -> Result<String, String> {
    let bare = model.strip_prefix(PREFIX).unwrap_or(model);
    if bare.contains('/') {
        Ok(bare.to_string())
    } else {
        Err(format!(
            "`{model}` is not an OpenRouter id: the shape is `openrouter:vendor/model` \
             (e.g. `openrouter:openai/gpt-5`)"
        ))
    }
}

/// The adapter this row registers.
pub struct OpenrouterAdapter {
    cfg: Arc<OpenrouterConfig>,
    /// `None` ⇒ build one per round through `bough_llm::client_for`. Tests inject a canned client
    /// here (the `llm-openai` seam).
    client: Option<Arc<dyn LlmClient>>,
}

impl OpenrouterAdapter {
    pub fn new(cfg: Arc<OpenrouterConfig>) -> OpenrouterAdapter {
        OpenrouterAdapter { cfg, client: None }
    }

    /// An adapter over a supplied client. Test seam only; the row never builds one of these.
    pub fn with_client(
        cfg: Arc<OpenrouterConfig>,
        client: Arc<dyn LlmClient>,
    ) -> OpenrouterAdapter {
        OpenrouterAdapter {
            cfg,
            client: Some(client),
        }
    }

    /// The client for one round. P2-D5: `bough-llm`'s own retries are DISABLED
    /// (`max_attempts: Some(1)`); retry is `llm-retry`'s waterfall listener.
    fn client_for(&self, model: &str) -> Arc<dyn LlmClient> {
        if let Some(c) = &self.client {
            return c.clone();
        }
        let cfg = self.cfg.clone();
        let env: bough_llm::routing::Env = Arc::new(move |name: &str| {
            // The key is read at CALL time from the configured variable (P2-D7), and a configured
            // base url is offered under the name the provider client looks for.
            if name == "OPENROUTER_API_KEY" {
                return std::env::var(&cfg.api_key_env).ok();
            }
            if name == "OPENROUTER_API_BASE" {
                if let Some(base) = &cfg.base_url {
                    return Some(base.clone());
                }
            }
            std::env::var(name).ok()
        });
        bough_llm::client_for(
            model,
            bough_llm::ClientOpts {
                provider: bough_llm::ProviderOpts {
                    env: Some(env),
                    transport: None,
                },
                retry: bough_llm::RetryOpts {
                    max_attempts: Some(1),
                    ..Default::default()
                },
                trace: None,
            },
        )
    }
}

#[async_trait::async_trait]
impl LlmAdapter for OpenrouterAdapter {
    fn name(&self) -> AdapterName {
        AdapterName::new(PLUGIN_NAME)
    }

    /// Never `Err` (§12): every exit is a terminal chunk. The `llm-openai` shape exactly, plus
    /// the prefix strip — the wire sees OpenRouter's own `vendor/model`, the durable header keeps
    /// the policy's `openrouter:…` spelling.
    async fn start(&self, req: Arc<LlmRequest>, cancel: CancellationToken) -> LlmStream {
        let mut params = adapt::request_to_params(&req);
        let name = AdapterName::new(PLUGIN_NAME);
        params.model = match wire_model(&params.model) {
            Ok(m) => m,
            Err(msg) => {
                let failed = Chunk::Failed(adapt::error_to_failure(
                    &name,
                    &bough_llm::error::LlmError::with(msg, 400, None),
                ));
                return Box::pin(futures::stream::once(async move { failed }));
            }
        };
        let client = self.client_for(&params.model);
        let timeout = std::time::Duration::from_millis(self.cfg.request_timeout_ms);

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Chunk>();
        let text_tx = tx.clone();
        let on_text: bough_llm::types::OnText = Arc::new(move |t: &str| {
            let _ = text_tx.send(Chunk::TextDelta {
                text: t.to_string(),
            });
        });

        let cancel2 = cancel.clone();
        tokio::spawn(async move {
            let round = tokio::time::timeout(timeout, client.run(params, on_text, cancel2)).await;
            let trailing = match round {
                Ok(Ok(result)) => adapt::round_to_chunks(&result),
                Ok(Err(e)) => vec![Chunk::Failed(adapt::error_to_failure(&name, &e))],
                Err(_) => vec![Chunk::Failed(adapt::error_to_failure(
                    &name,
                    &bough_llm::error::LlmError::with(
                        format!("no response within {}ms", timeout.as_millis()),
                        504,
                        None,
                    ),
                ))],
            };
            for c in trailing {
                let _ = tx.send(c);
            }
        });

        Box::pin(futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|c| (c, rx))
        }))
    }
}

/// The provider row.
pub struct OpenrouterPlugin;

#[async_trait::async_trait]
impl Plugin for OpenrouterPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = OpenrouterConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::required(["llm"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let llm = ctx
            .get::<Llm>()
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        // P2-D7: the API key is NOT read here. A credential is runtime state, and failing the
        // boot over one would make every offline test host unable to mount the row.
        llm.adapter(
            &ctx,
            AdapterSpec {
                name: AdapterName::new(PLUGIN_NAME),
                matches: ModelMatch::parse(&cfg.models),
                adapter: Arc::new(OpenrouterAdapter::new(cfg.clone())),
            },
        )
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<bough_kernel::InvariantSpec> {
        crate::invariant::specs()
    }
}

bough_kernel::register_plugin!(OpenrouterPlugin);

#[cfg(test)]
mod tests {
    use super::wire_model;

    #[test]
    fn the_prefix_strips_to_the_vendor_model_wire_spelling() {
        assert_eq!(
            wire_model("openrouter:openai/gpt-5").unwrap(),
            "openai/gpt-5"
        );
        assert_eq!(
            wire_model("qwen/qwen-3").unwrap(),
            "qwen/qwen-3",
            "an already-bare vendor/model id passes through"
        );
    }

    #[test]
    fn a_slashless_id_is_refused_by_name_never_misrouted() {
        let err = wire_model("openrouter:gpt-5").unwrap_err();
        assert!(err.contains("vendor/model"), "{err}");
    }
}
