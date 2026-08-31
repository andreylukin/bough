//! Invariant (§10): a worker's question is MAIL. It goes onto the spawner's lane as wake-class
//! mail with the wake flag set — the same durable splice any other sender gets — and the answer,
//! if one ever comes, comes back through this sink and nowhere else.

use std::collections::BTreeMap;
use std::sync::Arc;

use bough_plugin_agents::{AgentsHandle, Message, MessageId, Target};
use bough_plugin_ledger::AgentName;
use bough_plugin_workers::{AskSink, WorkerError};
use parking_lot::Mutex;
use tokio::sync::oneshot;

/// The production sink: the spawner's own `Agent::send`.
pub struct AgentsAskSink {
    agents: AgentsHandle,
    /// One waiter per outstanding question. A `block`-mode worker parks on its receiver; nothing
    /// in Phase 2 answers, so it ends — which is the honest behaviour until a surface can reply.
    waiting: Mutex<BTreeMap<MessageId, oneshot::Sender<String>>>,
}

impl AgentsAskSink {
    pub fn new(agents: AgentsHandle) -> AgentsAskSink {
        AgentsAskSink {
            agents,
            waiting: Mutex::new(BTreeMap::new()),
        }
    }

    /// Answer an outstanding question. `false` if nobody is waiting on it.
    pub fn answer_to(&self, msg: &MessageId, text: String) -> bool {
        match self.waiting.lock().remove(msg) {
            Some(tx) => tx.send(text).is_ok(),
            None => false,
        }
    }
}

#[async_trait::async_trait]
impl AskSink for AgentsAskSink {
    async fn deliver(
        &self,
        spawner: &AgentName,
        msg: Message,
        target: Target,
        wake: bool,
    ) -> Result<MessageId, WorkerError> {
        let agent = self
            .agents
            .by_name(spawner)
            .ok_or_else(|| bough_plugin_agents::AgentError::NoSuchAgent(spawner.clone()))?;
        let receipt = agent.send(msg, target, wake).await?;
        Ok(receipt.message)
    }

    /// The waiter is armed HERE and not in `deliver`, because only a `block`-mode worker parks:
    /// an `end`-mode worker never calls this, and arming a channel it will never read would leak
    /// one entry per question.
    async fn answer(&self, msg: &MessageId) -> Option<String> {
        let rx = {
            let (tx, rx) = oneshot::channel();
            self.waiting.lock().insert(msg.clone(), tx);
            rx
        };
        let out = rx.await.ok();
        self.waiting.lock().remove(msg);
        out
    }
}

/// A sink that records every delivery and answers from a script. The roundtrip test asserts on
/// the exact (spawner, message, target, wake) tuple that reaches the inbox.
pub type Delivery = (AgentName, Message, Target, bool);

pub struct RecordingAskSink {
    pub delivered: Arc<Mutex<Vec<Delivery>>>,
    pub reply: Option<String>,
}

impl RecordingAskSink {
    pub fn new(reply: Option<String>) -> RecordingAskSink {
        RecordingAskSink {
            delivered: Arc::new(Mutex::new(Vec::new())),
            reply,
        }
    }
}

#[async_trait::async_trait]
impl AskSink for RecordingAskSink {
    async fn deliver(
        &self,
        spawner: &AgentName,
        msg: Message,
        target: Target,
        wake: bool,
    ) -> Result<MessageId, WorkerError> {
        let id = msg.id.clone();
        self.delivered
            .lock()
            .push((spawner.clone(), msg, target, wake));
        Ok(id)
    }

    async fn answer(&self, _msg: &MessageId) -> Option<String> {
        self.reply.clone()
    }
}
