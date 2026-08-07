//! The `AgentRunner` seam — what one `agent()` call asks for, and the trait
//! that runs it (port of the `AgentCall`/`AgentRunner` half of
//! `src/workflow/run.ts`).
//!
//! This is THE injection point that keeps the whole engine — worker, journal,
//! semaphore, pause gate, replay — drivable offline with no LLM, no key and no
//! subagent. Production wires the subagent launcher behind it
//! (`workflow/control.rs`); every engine test injects a fake.
//!
//! Decorator order is part of the contract (ARCHITECTURE §7):
//! `SubagentRunner` → `StructuredRunner` → `ControlledRunner` (outermost).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::errors::BoughError;

/// What one `agent()` call asks for, parsed from the worker's bridged JSON.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentCall {
    pub prompt: String,
    /// The journal/display label. Never empty — defaulted from the prompt.
    pub label: String,
    pub phase: Option<String>,
    pub model: Option<String>,
    /// A JSON Schema. Opaque to the engine; part of what `key` hashes.
    pub schema: Option<Value>,
}

/// Called with the subagent session id the moment a call gets one, so the
/// journal row can point at the session while it is still running.
pub type OnSpawned = Arc<dyn Fn(&str) + Send + Sync>;

/// Runs one agent call to completion.
///
/// Resolves with the report VERBATIM — the string that lands in the journal and
/// comes back on a replay, so a replayed call and a live one are
/// indistinguishable to the script. MUST fail on failure: an `Err` is what
/// makes `parallel()` map the slot to `null` and `pipeline()` drop the item.
#[async_trait]
pub trait AgentRunner: Send + Sync {
    async fn run(
        &self,
        call: &AgentCall,
        cancel: CancellationToken,
        on_spawned: OnSpawned,
    ) -> Result<String, BoughError>;
}

/// A runner built from a closure — the shape every test and every decorator's
/// inner stub wants.
pub struct FnRunner<F>(pub F);

#[async_trait]
impl<F, Fut> AgentRunner for FnRunner<F>
where
    F: Fn(AgentCall, CancellationToken, OnSpawned) -> Fut + Send + Sync,
    Fut: std::future::Future<Output = Result<String, BoughError>> + Send,
{
    async fn run(
        &self,
        call: &AgentCall,
        cancel: CancellationToken,
        on_spawned: OnSpawned,
    ) -> Result<String, BoughError> {
        (self.0)(call.clone(), cancel, on_spawned).await
    }
}

/// The no-op `on_spawned` for callers that do not track sessions.
pub fn no_spawn_hook() -> OnSpawned {
    Arc::new(|_sid: &str| {})
}
