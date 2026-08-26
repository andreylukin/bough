//! Invariant: a plugin is a set of associated functions, not an object. The fiber owns the config
//! and the context, so the shim is a ZST and the catalog constructor is trivial. `validate` is
//! PURE and SYNCHRONOUS (§0.5); anything needing I/O or a clock belongs in `apply`.

use std::any::Any;
use std::sync::Arc;

use crate::config::Inject;
use crate::context::Context;
use crate::error::{ConfigError, PluginError};
use crate::invariant::InvariantSpec;

/// What a plugin crate implements.
#[async_trait::async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Catalog name; matches an entry's `plugin:` field.
    const NAME: &'static str;

    /// The row's validated configuration.
    type Config: serde::de::DeserializeOwned
        + serde::Serialize
        + schemars::JsonSchema
        + PartialEq
        + std::fmt::Debug
        + Send
        + Sync
        + 'static;

    /// Static injection declaration. Unioned with the entry's `inject:` field (Decision D1): the
    /// entry may ADD keys, it may not drop a plugin's static requirement.
    fn inject() -> Inject {
        Inject::none()
    }

    /// PURE, SYNCHRONOUS validation (§0.5). No I/O, no clock, no network.
    fn validate(_cfg: &Self::Config) -> Result<(), ConfigError> {
        Ok(())
    }

    /// Register the plugin's effects. Returning `Ok` means ACTIVE. Everything registered here is
    /// an effect of this fiber and is unwound on unload.
    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError>;

    /// A new config is always HANDED to the plugin; the plugin decides whether it is material
    /// (§0.3). Default: material iff `old != new` (Decision D7).
    fn reconfigure(_ctx: &Context, old: &Self::Config, new: &Self::Config) -> Reconfigure {
        if old == new {
            Reconfigure::Applied
        } else {
            Reconfigure::Reload
        }
    }

    /// §0.2: every plugin crate owns an invariant module, or states why it has none.
    fn invariants() -> Vec<InvariantSpec> {
        Vec::new()
    }
}

/// The plugin's verdict on a config diff.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Reconfigure {
    /// Absorbed live; no reload.
    Applied,
    /// Unload, then load with the new config.
    Reload,
}

/// The object-safe boundary the catalog and the fiber driver speak.
///
/// `impl<P: Plugin> ErasedPlugin for Shim<P>` is blanket and written once, in this module.
pub trait ErasedPlugin: Send + Sync + 'static {
    fn name(&self) -> &'static str;
    fn inject(&self) -> Inject;
    fn schema(&self) -> schemars::Schema;
    /// Deserialize + [`Plugin::validate`]. The returned handle carries both the typed value and
    /// the canonicalised YAML used by `--dump-config` and by the fingerprint.
    fn parse(&self, raw: &serde_yaml::Value) -> Result<ErasedConfig, ConfigError>;
    fn apply(
        &self,
        ctx: Context,
        cfg: ErasedConfig,
    ) -> futures::future::BoxFuture<'static, Result<(), PluginError>>;
    fn reconfigure(&self, ctx: &Context, old: &ErasedConfig, new: &ErasedConfig) -> Reconfigure;
    fn invariants(&self) -> Vec<InvariantSpec>;
}

/// A parsed config, type-erased: the typed value for the plugin, the canonical YAML for the dump
/// and the fingerprint.
#[derive(Clone)]
pub struct ErasedConfig {
    typed: Arc<dyn Any + Send + Sync>,
    yaml: Arc<serde_yaml::Value>,
}

impl ErasedConfig {
    /// Wrap a typed config and its canonical YAML.
    pub fn new<T: Any + Send + Sync>(typed: T, yaml: serde_yaml::Value) -> Self {
        Self {
            typed: Arc::new(typed),
            yaml: Arc::new(yaml),
        }
    }
    /// Recover the typed value. `None` if `T` is not this row's config type.
    pub fn downcast<T: Any + Send + Sync>(&self) -> Option<Arc<T>> {
        self.typed.clone().downcast::<T>().ok()
    }
    /// The canonical YAML this config parsed from.
    pub fn yaml(&self) -> &serde_yaml::Value {
        &self.yaml
    }
}

/// The ZST that carries a `P: Plugin` into the object-safe world.
pub struct Shim<P: Plugin>(std::marker::PhantomData<fn() -> P>);

impl<P: Plugin> Shim<P> {
    pub fn new() -> Self {
        Self(std::marker::PhantomData)
    }
}

impl<P: Plugin> Default for Shim<P> {
    fn default() -> Self {
        Self::new()
    }
}

impl<P: Plugin> ErasedPlugin for Shim<P> {
    fn name(&self) -> &'static str {
        P::NAME
    }
    fn inject(&self) -> Inject {
        P::inject()
    }
    fn schema(&self) -> schemars::Schema {
        todo!("WP-3: schemars::schema_for!(P::Config)")
    }
    fn parse(&self, raw: &serde_yaml::Value) -> Result<ErasedConfig, ConfigError> {
        todo!("WP-3: deserialize into P::Config, run P::validate, canonicalise the yaml")
    }
    fn apply(
        &self,
        ctx: Context,
        cfg: ErasedConfig,
    ) -> futures::future::BoxFuture<'static, Result<(), PluginError>> {
        todo!("WP-3: downcast and box P::apply")
    }
    fn reconfigure(&self, ctx: &Context, old: &ErasedConfig, new: &ErasedConfig) -> Reconfigure {
        todo!("WP-3: downcast both and call P::reconfigure")
    }
    fn invariants(&self) -> Vec<InvariantSpec> {
        P::invariants()
    }
}
