//! Invariant (§2): this is the `AgentFactory` / `AgentDriver` the `agents` seam's factory slot
//! holds. Everything the loop knows about scheduling lives behind these four methods, so a
//! replacement loop (`agent-loop-scripted`) is held to the ledger protocol and not to a feature
//! list.

use std::sync::Arc;

use bough_plugin_agents::{
    AgentCell, AgentDriver, AgentError, AgentFactory, Attach, CancelCause, InboxReceipt, Message,
};

use crate::LoopConfig;

/// The factory this row registers.
pub struct LoopFactory {
    _cfg: Arc<LoopConfig>,
}

impl LoopFactory {
    /// WP-4.
    pub fn new(cfg: Arc<LoopConfig>) -> LoopFactory {
        LoopFactory { _cfg: cfg }
    }
}

#[async_trait::async_trait]
impl AgentFactory for LoopFactory {
    fn driver(&self) -> &'static str {
        crate::PLUGIN_NAME
    }

    /// WP-4: start this agent's scheduler task, and hand back its driver.
    async fn attach(
        &self,
        _cell: AgentCell,
        _mode: Attach,
    ) -> Result<Arc<dyn AgentDriver>, AgentError> {
        todo!("WP-4: spawn the per-agent scheduler and return the driver")
    }
}

/// One agent's running loop.
pub struct LoopDriver {
    /// WP-4 fills this in.
    _cfg: Arc<LoopConfig>,
}

#[async_trait::async_trait]
impl AgentDriver for LoopDriver {
    fn driver(&self) -> &'static str {
        crate::PLUGIN_NAME
    }

    /// IMMEDIATE for an Andrey message or wake-class mail; a debounced drain otherwise, with one
    /// drain wake in flight per agent (§5).
    ///
    /// WP-4.
    async fn notify(&self, _receipt: &InboxReceipt, _msg: &Message) {
        todo!("WP-4: urgency -> schedule, or join the debounce window")
    }

    /// WP-4.
    async fn cancel(&self, _cause: CancelCause, _keep_inbox: bool) {
        todo!("WP-4: fire the token; the running wake ends as aborted with this cause")
    }

    /// Stop and drain: no new wake starts, the in-flight wake ends, returns when idle.
    ///
    /// WP-4.
    async fn stop(&self) {
        todo!("WP-4")
    }
}
