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
    adapters: parking_lot::Mutex<Vec<AdapterSpec>>,
    /// The fiber the recorder attributes observations to, so a reload starts clean.
    fiber: parking_lot::Mutex<bough_kernel::FiberUid>,
}

impl LlmHandle {
    /// An empty seam.
    pub fn new() -> LlmHandle {
        LlmHandle(Arc::new(LlmInner {
            adapters: parking_lot::Mutex::new(Vec::new()),
            fiber: parking_lot::Mutex::new(bough_kernel::FiberUid(0)),
        }))
    }

    /// Register an adapter. Registration is an effect: unloading the provider row removes it.
    pub async fn adapter(
        &self,
        ctx: &Context,
        spec: AdapterSpec,
    ) -> Result<EffectHandle, PluginError> {
        let inner = self.0.clone();
        let name = spec.name.clone();
        ctx.effect(move |e| async move {
            inner.adapters.lock().push(spec);
            let inner2 = inner.clone();
            e.defer_sync(move || {
                inner2.adapters.lock().retain(|s| s.name != name);
            });
            Ok(())
        })
        .await
    }

    /// Explicit `resolve(request) -> Spec` (§0.2): most specific match wins; a tie is
    /// [`LlmSeamError::AmbiguousAdapter`] naming both.
    pub fn resolve(&self, model: &str) -> Result<Arc<dyn LlmAdapter>, LlmSeamError> {
        let adapters = self.0.adapters.lock();
        let mut claimants: Vec<&AdapterSpec> = adapters
            .iter()
            .filter(|s| s.matches.claims(model))
            .collect();
        // Deterministic: by specificity, then by name, so the tie report names the same two
        // adapters whatever order the rows activated in.
        claimants.sort_by(|a, b| {
            b.matches
                .specificity()
                .cmp(&a.matches.specificity())
                .then_with(|| a.name.as_str().cmp(b.name.as_str()))
        });
        match claimants.as_slice() {
            [] => Err(LlmSeamError::NoAdapter {
                model: model.to_string(),
                registered: adapters.iter().map(|s| s.name.to_string()).collect(),
            }),
            [only] => Ok(only.adapter.clone()),
            [a, b, ..] if a.matches.specificity() == b.matches.specificity() => {
                Err(LlmSeamError::AmbiguousAdapter {
                    model: model.to_string(),
                    a: a.name.clone(),
                    b: b.name.clone(),
                })
            }
            [best, ..] => Ok(best.adapter.clone()),
        }
    }

    /// Run the `llm/stream` waterfall and hand back the stream. A missing adapter and an adapter
    /// failure are both `Chunk::Failed`, so no caller branches on two failure shapes.
    ///
    /// The resolved adapter is the chain's INNERMOST hop, and it gets there the only way the
    /// kernel allows: a listener registered for the duration of this one call, which is therefore
    /// registered last and runs last. That is what makes "a wrapper that returns without calling
    /// `next` and without filling the slot" DISTINGUISHABLE from "nobody wrapped" — the slot is
    /// still empty, and an empty slot is a `Failed` chunk rather than a hang.
    pub async fn stream(
        &self,
        ctx: &Context,
        req: Arc<LlmRequest>,
        cancel: CancellationToken,
    ) -> LlmStream {
        let inner = self.0.clone();
        let mine = req.clone();
        let hop = ctx
            .on_waterfall::<LlmStreamEvent, _, _>(move |c: StreamCall, next| {
                let inner = inner.clone();
                let mine = mine.clone();
                async move {
                    // Another call's dispatch: delegate, so its own innermost hop serves it.
                    if !Arc::ptr_eq(&c.request, &mine) {
                        return next.run(c).await;
                    }
                    match inner_resolve(&inner, &c.request.model) {
                        Ok(adapter) => c
                            .stream
                            .put(adapter.start(c.request.clone(), c.cancel.clone()).await),
                        Err(e) => c.stream.put(failed_stream(seam_failure(&e))),
                    }
                    c
                }
            })
            .await;

        let call = StreamCall {
            request: req.clone(),
            cancel,
            stream: StreamSlot::empty(),
        };
        let out = match &hop {
            Ok(_) => ctx.waterfall::<LlmStreamEvent>(call).await,
            // The seam could not register its own hop; the request cannot be served, and saying so
            // as a chunk keeps the one failure shape.
            Err(e) => {
                let msg = e.to_string();
                let c = call;
                c.stream.put(failed_stream(LlmFailure {
                    kind: FailureKind::Other,
                    message: msg,
                    retryable: false,
                    status: None,
                    adapter: AdapterName::new(PLUGIN_NAME),
                }));
                c
            }
        };
        if let Ok(h) = hop {
            h.dispose().await;
        }

        let stream = out.stream.take().unwrap_or_else(|| {
            failed_stream(LlmFailure {
                kind: FailureKind::BadRequest,
                message: format!(
                    "an `llm/stream` listener short-circuited the chain for model `{}` without \
                     supplying a stream; no model round was made",
                    req.model
                ),
                retryable: false,
                status: None,
                adapter: AdapterName::new(PLUGIN_NAME),
            })
        });
        // Every stream the seam hands out is watched, so the invariant judges what CONSUMERS saw
        // and not what an adapter meant to produce.
        watch(stream, *self.0.fiber.lock(), req.digest())
    }

    /// Every registered adapter, for `--dump-config` and for error messages.
    pub fn adapters(&self) -> Vec<(AdapterName, ModelMatch)> {
        self.0
            .adapters
            .lock()
            .iter()
            .map(|s| (s.name.clone(), s.matches.clone()))
            .collect()
    }
}

impl Default for LlmHandle {
    fn default() -> Self {
        LlmHandle::new()
    }
}

fn inner_resolve(inner: &Arc<LlmInner>, model: &str) -> Result<Arc<dyn LlmAdapter>, LlmSeamError> {
    LlmHandle(inner.clone()).resolve(model)
}

/// §12: "a missing adapter is a chunk too, so no caller has to branch on two failure shapes."
fn seam_failure(e: &LlmSeamError) -> LlmFailure {
    LlmFailure {
        kind: FailureKind::BadRequest,
        message: e.to_string(),
        retryable: false,
        status: None,
        adapter: AdapterName::new(PLUGIN_NAME),
    }
}

/// A one-chunk stream carrying a terminal failure.
pub fn failed_stream(failure: LlmFailure) -> LlmStream {
    Box::pin(futures::stream::once(async move { Chunk::Failed(failure) }))
}

/// Wrap a stream so the seam's invariant sees the chunk shape a CONSUMER actually received.
///
/// The observation is filed when the wrapped stream is dropped, which is the only moment at which
/// "nothing followed the terminal chunk" is knowable. A stream abandoned before it terminated
/// (a cancelled wake) files nothing: the invariant is about what a producer emitted, and a
/// consumer that stopped listening has not caught it doing anything.
fn watch(inner: LlmStream, fiber: bough_kernel::FiberUid, request: String) -> LlmStream {
    use futures::StreamExt;
    let mut guard = Recorder {
        obs: invariant::Obs {
            fiber,
            request,
            terminals: 0,
            after_terminal: 0,
        },
    };
    Box::pin(inner.map(move |chunk| {
        // Capture the WHOLE guard, not its fields: edition-2021 disjoint capture would otherwise
        // split it and its `Drop` would never file the observation.
        let guard = &mut guard;
        if guard.obs.terminals > 0 {
            guard.obs.after_terminal += 1;
        }
        if chunk.is_terminal() {
            guard.obs.terminals += 1;
        }
        chunk
    }))
}

/// Files the observation when the wrapped stream is dropped, exactly once.
struct Recorder {
    obs: invariant::Obs,
}

impl Drop for Recorder {
    fn drop(&mut self) {
        if self.obs.terminals == 0 && self.obs.after_terminal == 0 {
            return;
        }
        invariant::record(self.obs.clone());
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

    async fn apply(ctx: Context, _cfg: Arc<Self::Config>) -> Result<(), PluginError> {
        let handle = LlmHandle::new();
        *handle.0.fiber.lock() = ctx.fiber_uid();
        ctx.provide::<Llm>(handle)
            .await
            .map_err(|e| PluginError::new(ctx.entry_id().clone(), e))?;
        // A reload keeps the FiberUid, so the record is cleared rather than accumulated.
        let fiber = ctx.fiber_uid();
        ctx.effect(move |e| async move {
            e.defer_sync(move || invariant::forget(fiber));
            Ok(())
        })
        .await?;
        Ok(())
    }

    fn invariants() -> Vec<InvariantSpec> {
        vec![invariant::every_stream_ends_once()]
    }
}

bough_kernel::register_plugin!(LlmPlugin);
