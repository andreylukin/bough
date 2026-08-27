//! Invariant: this crate is the `js` SERVICE DEFINITION (phase codemode §2.1). It owns the `js`
//! key, the `Program`/`HostFn`/`Caps` vocabulary and the engine factory slot — and not one line
//! of engine code, no I/O of its own, and no domain vocabulary. Everything a program can reach
//! is in `Program::host`.

pub mod engine;
pub mod error;
pub mod invariant;
pub mod program;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey};

pub use engine::JsEngine;
pub use error::JsError;
pub use program::{Caps, ConsoleSink, HostCall, HostFn, HostRefusal, Program, RefusalKind, Run};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "js";

/// The `js` service key.
pub struct Js;

impl ServiceKey for Js {
    type Value = JsHandle;
    const NAME: &'static str = "js";
}

/// The concrete handle the key's value is.
#[derive(Clone)]
pub struct JsHandle(pub Arc<JsInner>);

/// The seam's live state: the one engine, and the caps a program gets when it names none.
pub struct JsInner {
    engine: parking_lot::Mutex<Option<Arc<dyn JsEngine>>>,
    default_caps: Caps,
}

impl JsHandle {
    /// No `new()` and no `Default`: `Caps` are deployment-varying and [`JsConfig`] is their one
    /// source (§0.2), exactly as `ToolsHandle::with_limits` is spelled.
    pub fn with_caps(default: Caps) -> JsHandle {
        JsHandle(Arc::new(JsInner {
            engine: parking_lot::Mutex::new(None),
            default_caps: default,
        }))
    }

    /// The factory slot, in the shape of `ctx.agents.set_factory` (§2): a SECOND engine is an
    /// ERROR, not a silent replacement. Registration is an effect; the disposer clears the slot.
    ///
    /// WP-1 owns the body.
    pub async fn set_engine(
        &self,
        _ctx: &Context,
        _e: Arc<dyn JsEngine>,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-1: install the engine as an effect; a second engine is JsError::NoEngine's twin")
    }

    /// The installed engine, if a Provider row set one.
    pub fn engine(&self) -> Option<Arc<dyn JsEngine>> {
        self.0.engine.lock().clone()
    }

    /// The caps a `Program` gets when its caller names none.
    pub fn default_caps(&self) -> Caps {
        self.0.default_caps
    }

    /// Compile-only. The parse comes from the engine that will RUN the program, so host and
    /// engine can never disagree about what is legal (main's `check` message).
    ///
    /// WP-1 owns the body.
    pub async fn check(&self, _src: &str) -> Result<(), JsError> {
        todo!("WP-1: delegate to the installed engine; no engine ⇒ JsError::NoEngine")
    }

    /// Run one program to its single terminal outcome.
    ///
    /// WP-1 owns the body.
    pub async fn run(&self, _p: Program) -> Result<Run, JsError> {
        todo!("WP-1: delegate to the installed engine; no engine ⇒ JsError::NoEngine")
    }
}

/// The row's config: the one source of the default caps.
#[derive(
    Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct JsConfig {
    pub default_caps: Caps,
}

/// The Service Definition row.
pub struct JsPlugin;

#[async_trait::async_trait]
impl Plugin for JsPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = JsConfig;

    fn inject() -> bough_kernel::Inject {
        bough_kernel::Inject::none()
    }

    fn validate(cfg: &Self::Config) -> Result<(), bough_kernel::ConfigError> {
        let c = &cfg.default_caps;
        if c.ops == 0 || c.memory_bytes == 0 || c.stack_bytes == 0 || c.wall_ms == 0 {
            return Err(bough_kernel::ConfigError::Rejected {
                detail: "every cap must be at least 1: a zero cap is a program that cannot run"
                    .to_string(),
            });
        }
        Ok(())
    }

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: provide the `js` key with JsHandle::with_caps(cfg.default_caps)")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::exactly_one_terminal_outcome()]
    }
}

bough_kernel::register_plugin!(JsPlugin);
