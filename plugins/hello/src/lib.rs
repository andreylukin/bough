//! Invariant: `hello` is a TEST INSTRUMENT, not a product row. It exists to prove the kernel
//! (§17 Phase 0) and every interesting moment pushes a line onto a shared trace the tests assert
//! on in order. It is deleted or demoted at Phase 8.
//!
//! Three catalog names live here (Decision D14, a deliberate exception to one-crate-one-row):
//! `hello` (the consumer, `inject: ["greeting"]`), `greeting-echo` and `greeting-shout` (two
//! providers of the same key whose greeting output is EQUAL — which is what makes V2 meaningful:
//! hello reloads because the provider fiber changed, not because the value did).
//!
//! SCAFFOLD: remove this allow when the bodies land (WP-6).
#![allow(unused_variables, dead_code)]

pub mod invariant;
pub mod provider;

use std::sync::Arc;

use bough_kernel::{
    Context, Inject, InvariantSpec, Plugin, PluginError, Reconfigure, ServiceKey,
};

pub use provider::{EchoProvider, ShoutProvider};

/// The Service Definition: the one key Phase 0 defines anywhere.
pub struct Greeting;

impl ServiceKey for Greeting {
    type Value = GreetingHandle;
    const NAME: &'static str = "greeting";
}

/// The concrete handle newtype the key's value is (Decision D5: `ServiceKey::Value` is `Sized`,
/// so a trait-object service is wrapped by the Service Definition that owns it).
#[derive(Clone)]
pub struct GreetingHandle(pub Arc<dyn GreetingSink>);

/// What a greeting provider does.
pub trait GreetingSink: Send + Sync + 'static {
    fn greet(&self, who: &str) -> String;
    /// The catalog name of the plugin behind this binding; the swap test asserts on it.
    fn provider(&self) -> &'static str;
}

/// How chatty `hello` is. Immaterial: a change to this field is absorbed by `reconfigure` without
/// a reload, which is what proves "config is handed to the plugin, which reloads only on a
/// material diff" (§0.3).
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize,
    schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    #[default]
    Info,
    Warn,
}

/// The consumer row's config.
#[derive(Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
pub struct HelloConfig {
    /// Material: a change reloads the fiber.
    pub who: String,
    /// Immaterial: absorbed by `reconfigure`.
    #[serde(default)]
    pub log_level: LogLevel,
    /// Test hook (Decision D16): makes the invariant fail on purpose. V9's vehicle.
    #[serde(default)]
    pub plant_violation: bool,
    /// Test hook (Decision D16): `apply` reads a key it never declared. V8's vehicle.
    #[serde(default)]
    pub read_undeclared: Option<String>,
}

/// The consumer.
pub struct HelloPlugin;

#[async_trait::async_trait]
impl Plugin for HelloPlugin {
    const NAME: &'static str = "hello";
    type Config = HelloConfig;

    fn inject() -> Inject {
        Inject::required(["greeting"])
    }

    async fn apply(ctx: Context, cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-6: read `greeting` from the committed view, register the greeted-seq effect, trace")
    }

    /// `Applied` when only `log_level` differs; `Reload` otherwise.
    fn reconfigure(ctx: &Context, old: &Self::Config, new: &Self::Config) -> Reconfigure {
        todo!("WP-6")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::greeted_seq_is_monotonic()]
    }
}

/// A greeting was emitted. The stream `plugins/hello/src/invariant.rs` polices.
#[derive(Clone, Debug)]
pub struct Greeted {
    pub fiber: bough_kernel::FiberUid,
    pub seq: u64,
    pub text: String,
}

/// `hello/greeted`.
pub struct GreetedEvent;

impl bough_kernel::EmitEvent for GreetedEvent {
    const NAME: &'static str = "hello/greeted";
    type Payload = Greeted;
}

/// The observation channel the Phase 0 tests assert on, in order: `("hello", "apply")`,
/// `("greeting-echo", "withdraw")`, and so on.
pub mod trace {
    use parking_lot::Mutex;
    use std::sync::Arc;

    /// A shared, ordered log of `(plugin, moment)` pairs.
    #[derive(Clone, Default)]
    pub struct Trace(Arc<Mutex<Vec<(&'static str, &'static str)>>>);

    impl Trace {
        pub fn new() -> Self {
            Self::default()
        }
        pub fn push(&self, plugin: &'static str, moment: &'static str) {
            self.0.lock().push((plugin, moment));
        }
        pub fn lines(&self) -> Vec<(&'static str, &'static str)> {
            self.0.lock().clone()
        }
    }

    /// The process-wide trace the fixture writes to. A test clears it before booting.
    pub fn global() -> Trace {
        todo!("WP-6")
    }
}

bough_kernel::register_plugin!(HelloPlugin);
bough_kernel::register_plugin!(EchoProvider);
bough_kernel::register_plugin!(ShoutProvider);
