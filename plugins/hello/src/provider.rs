//! Invariant: the two providers produce EQUAL greeting text. The Phase 0 swap test is only
//! meaningful if the observable value does not change when the provider row does — what reloads
//! `hello` is the `ProviderUid`, never the value (§0.3, V2).

use std::sync::Arc;

use bough_kernel::{Context, Inject, Plugin, PluginError};

/// The default provider (`greeting-echo`), the row `bundles/bough-base.yml` ships.
pub struct EchoProvider;

/// One provider's config: a suffix appended to the greeting.
#[derive(Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct ProviderConfig {
    #[serde(default)]
    pub suffix: String,
}

#[async_trait::async_trait]
impl Plugin for EchoProvider {
    const NAME: &'static str = "greeting-echo";
    type Config = ProviderConfig;

    fn inject() -> Inject {
        Inject::none()
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: provide Greeting with a sink whose provider() is \"greeting-echo\"")
    }
}

/// The swap target (`greeting-shout`): a second plugin providing the same key with equal output.
pub struct ShoutProvider;

#[async_trait::async_trait]
impl Plugin for ShoutProvider {
    const NAME: &'static str = "greeting-shout";
    type Config = ProviderConfig;

    fn inject() -> Inject {
        Inject::none()
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: provide Greeting with a sink whose provider() is \"greeting-shout\"")
    }
}
