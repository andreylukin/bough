//! Invariant: this crate is the llm SERVICE DEFINITION (§12). It owns the `llm` key, the message
//! and stream vocabulary, the adapter registry and the three waterfalls (`agent/request`,
//! `agent/request-error`, `llm/stream`) — and not one line of provider code. A model failure
//! leaves this seam as a terminal chunk, never as an `Err`.
//!
//! P2-D1: it owns live state (the adapter map), so it IS a catalog row and provides its own key.

pub mod adapter;
pub mod error;
pub mod ids;
pub mod invariant;
pub mod request;
pub mod stream;

use std::sync::Arc;

use bough_kernel::{Context, EffectHandle, InvariantSpec, Plugin, PluginError, ServiceKey};
use tokio_util::sync::CancellationToken;

pub use adapter::{AdapterSpec, LlmAdapter, ModelMatch};
pub use error::LlmSeamError;
pub use ids::{AdapterName, ToolCallId, ToolName};
pub use request::{
    AgentRequest, AgentRequestError, CallConfig, Effort, LlmContentBlock, LlmMessage, LlmRequest,
    LlmRole, LlmToolDef, Recovery, RequestCall, RequestErrorCall, RequestFacts, WakeKind,
};
pub use stream::{
    Chunk, FailureKind, LlmFailure, LlmStream, LlmStreamEvent, StopReason, StreamCall, StreamSlot,
    Usage,
};

/// The catalog name of this row.
pub const PLUGIN_NAME: &str = "llm";

/// The `llm` service key.
pub struct Llm;

impl ServiceKey for Llm {
    type Value = LlmHandle;
    const NAME: &'static str = "llm";
}

/// The concrete handle the key's value is (Decision D5).
#[derive(Clone)]
pub struct LlmHandle(pub Arc<LlmInner>);

/// The seam's live state: the adapter map. Private by construction — every mutation goes through
/// [`LlmHandle::adapter`], which is an effect (§0.2).
pub struct LlmInner {
    /// WP-1 fills this in. Named so the shape of the state is visible in the scaffold.
    _adapters: parking_lot::Mutex<Vec<AdapterSpec>>,
}

impl LlmHandle {
    /// An empty seam. WP-1.
    pub fn new() -> LlmHandle {
        LlmHandle(Arc::new(LlmInner {
            _adapters: parking_lot::Mutex::new(Vec::new()),
        }))
    }

    /// Register an adapter. Registration is an effect: unloading the provider row removes it.
    ///
    /// WP-1.
    pub async fn adapter(
        &self,
        _ctx: &Context,
        _spec: AdapterSpec,
    ) -> Result<EffectHandle, PluginError> {
        todo!("WP-1: register the adapter as an effect of the calling fiber")
    }

    /// Explicit `resolve(request) -> Spec` (§0.2): most specific match wins; a tie is
    /// [`LlmSeamError::AmbiguousAdapter`] naming both.
    ///
    /// WP-1.
    pub fn resolve(&self, _model: &str) -> Result<Arc<dyn LlmAdapter>, LlmSeamError> {
        todo!("WP-1: most-specific-wins adapter resolution")
    }

    /// Run the `llm/stream` waterfall and hand back the stream. A missing adapter and an adapter
    /// failure are both `Chunk::Failed`, so no caller branches on two failure shapes.
    ///
    /// WP-1.
    pub async fn stream(
        &self,
        _ctx: &Context,
        _req: Arc<LlmRequest>,
        _cancel: CancellationToken,
    ) -> LlmStream {
        todo!("WP-1: waterfall llm/stream, innermost hop = resolve(model).start(..)")
    }

    /// Every registered adapter, for `--dump-config` and for error messages.
    ///
    /// WP-1.
    pub fn adapters(&self) -> Vec<(AdapterName, ModelMatch)> {
        todo!("WP-1: list the registered adapters")
    }
}

impl Default for LlmHandle {
    fn default() -> Self {
        LlmHandle::new()
    }
}

/// No configuration: the seam holds no deployment-varying value; the adapters do.
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LlmConfig {}

/// The Service Definition row.
pub struct LlmPlugin;

#[async_trait::async_trait]
impl Plugin for LlmPlugin {
    const NAME: &'static str = PLUGIN_NAME;
    type Config = LlmConfig;

    async fn apply(_ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        todo!("WP-1: provide::<Llm>(LlmHandle::new()) and record the stream for the invariant")
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::every_stream_ends_once()]
    }
}

bough_kernel::register_plugin!(LlmPlugin);
