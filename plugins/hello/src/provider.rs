//! Invariant: the two providers produce EQUAL greeting text. The Phase 0 swap test is only
//! meaningful if the observable value does not change when the provider row does — what reloads
//! `hello` is the `ProviderUid`, never the value (§0.3, V2).

use std::sync::Arc;

use bough_kernel::{Context, Inject, Plugin, PluginError, ServiceSlot};
use parking_lot::Mutex;

use crate::{trace, GreetedEvent, Greeting, GreetingHandle, GreetingSink};

/// One provider's config: a suffix appended to the greeting.
#[derive(
    Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
pub struct ProviderConfig {
    #[serde(default)]
    pub suffix: String,
}

/// The sink both providers install. `provider` is the only thing that differs between them; the
/// text they produce is byte-identical, on purpose.
struct Sink {
    suffix: String,
    provider: &'static str,
}

impl GreetingSink for Sink {
    fn greet(&self, who: &str) -> String {
        format!("hello, {who}{}", self.suffix)
    }
    fn provider(&self) -> &'static str {
        self.provider
    }
}

/// The most recently installed slot, so a test can call `set` / `republish` on a live binding
/// (`provider_in_place_set_is_not_observed_by_hello`). A fixture affordance; no product row does
/// anything like this.
static LAST_SLOT: Mutex<Option<Arc<ServiceSlot<Greeting>>>> = Mutex::new(None);

/// The slot the most recent provider activation installed.
pub fn last_slot() -> Option<Arc<ServiceSlot<Greeting>>> {
    LAST_SLOT.lock().clone()
}

/// Forget the remembered slot. Called from `trace::test_lock`'s neighbours in a test's setup.
pub fn forget_slot() {
    *LAST_SLOT.lock() = None;
}

/// The body both providers share: trace, provide, remember the slot, trace on the way out.
async fn provide_greeting(
    ctx: Context,
    cfg: Arc<ProviderConfig>,
    name: &'static str,
) -> Result<(), PluginError> {
    let t = trace::global();
    t.push(name, "apply");
    let entry = ctx.entry_id().clone();

    let slot = ctx
        .provide::<Greeting>(GreetingHandle(Arc::new(Sink {
            suffix: cfg.suffix.clone(),
            provider: name,
        })))
        .await
        .map_err(|e| PluginError::new(entry, e))?;
    *LAST_SLOT.lock() = Some(Arc::new(slot));

    // A listener owned by THIS fiber. It does nothing; it exists so that a swap test can assert
    // on `listener_count` directly — "the retired provider leaks no listeners" is otherwise only
    // observable indirectly.
    ctx.on_parallel::<GreetedEvent, _, _>(|_g| async move {})
        .await?;

    ctx.effect(move |e| async move {
        e.defer_sync(move || t.push(name, "unload"));
        Ok(())
    })
    .await?;
    Ok(())
}

/// The default provider (`greeting-echo`), the row `bundles/bough-base.yml` ships.
pub struct EchoProvider;

#[async_trait::async_trait]
impl Plugin for EchoProvider {
    const NAME: &'static str = "greeting-echo";
    type Config = ProviderConfig;

    fn inject() -> Inject {
        Inject::none()
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        provide_greeting(ctx, cfg, Self::NAME).await
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
        provide_greeting(ctx, cfg, Self::NAME).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The claim the swap test rests on: swapping the provider row changes the binding identity,
    /// not the value. If these ever diverge, `hello_reloads_when_a_different_fiber_provides_an
    /// _equal_value` would be proving the wrong thing.
    #[test]
    fn the_two_providers_produce_equal_text() {
        let echo = Sink {
            suffix: "".into(),
            provider: EchoProvider::NAME,
        };
        let shout = Sink {
            suffix: "".into(),
            provider: ShoutProvider::NAME,
        };
        assert_eq!(echo.greet("world"), shout.greet("world"));
        assert_ne!(echo.provider(), shout.provider());
    }
}
