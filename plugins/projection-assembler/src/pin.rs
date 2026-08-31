//! Invariant (P5-D12, §10): a PINNED prefix is returned VERBATIM. While a pin stands for an agent,
//! `assemble` for that agent is not an assembly at all — it is a replay of bytes someone else
//! already assembled, whatever the request's budget or `as_of` says. The pin is per-agent (one
//! child's pin is invisible to every other agent), and removing it restores ordinary assembly, so
//! the store holds no memory of an agent that has unpinned.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use bough_plugin_ledger::AgentName;
use bough_plugin_projection::{Assembled, PrefixSource, PrefixToken};
use std::sync::Arc;

/// One standing pin. `serial` is what a disposer names: pinning twice for the same agent and
/// disposing the FIRST token must not take the second pin with it.
#[derive(Clone, Debug)]
pub struct Pinned {
    pub serial: u64,
    pub prefix: Assembled,
    pub source: PrefixSource,
}

/// Every standing pin, by agent.
#[derive(Default)]
pub struct PinStore {
    inner: parking_lot::RwLock<HashMap<AgentName, Pinned>>,
    next: AtomicU64,
}

impl PinStore {
    /// Pin `prefix` for `agent`, and hand back the disposer that unpins it.
    pub fn pin(
        self: &Arc<Self>,
        agent: AgentName,
        prefix: Assembled,
        source: PrefixSource,
    ) -> PrefixToken {
        let serial = self.next.fetch_add(1, Ordering::SeqCst);
        self.inner.write().insert(
            agent.clone(),
            Pinned {
                serial,
                prefix,
                source,
            },
        );
        let store = Arc::clone(self);
        PrefixToken::new(move || {
            let mut guard = store.inner.write();
            // Idempotent, and serial-checked: a later pin for the same agent survives an earlier
            // token's disposal.
            if guard.get(&agent).map(|p| p.serial) == Some(serial) {
                guard.remove(&agent);
            }
        })
    }

    /// The prefix pinned for `agent`, if any.
    pub fn get(&self, agent: &AgentName) -> Option<Assembled> {
        self.inner.read().get(agent).map(|p| p.prefix.clone())
    }

    /// Where the standing pin for `agent` came from. Read by the invariant, not by assembly.
    pub fn source(&self, agent: &AgentName) -> Option<PrefixSource> {
        self.inner.read().get(agent).map(|p| p.source.clone())
    }

    /// How many pins stand. A disposed pin leaves NOTHING behind (§0.2).
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bough_plugin_ledger::Seq;
    use bough_plugin_projection::SectionCites;

    fn assembled(agent: &str) -> Assembled {
        Assembled {
            agent: AgentName::new(agent),
            sections: Vec::new(),
            flags: Default::default(),
            tokens: 0,
            budget: 10,
            cites: SectionCites::default(),
        }
    }

    fn source() -> PrefixSource {
        PrefixSource {
            of_agent: AgentName::new("sol"),
            as_of: Seq(7),
        }
    }

    #[test]
    fn a_pin_is_per_agent_and_disposal_leaves_nothing() {
        let store = Arc::new(PinStore::default());
        let token = store.pin(AgentName::new("child"), assembled("sol"), source());
        assert!(store.get(&AgentName::new("child")).is_some());
        assert!(store.get(&AgentName::new("other")).is_none());
        token.remove();
        assert!(store.is_empty(), "a disposed pin leaves no trace");
        token.remove();
    }

    #[test]
    fn an_earlier_tokens_disposal_does_not_take_a_later_pin() {
        let store = Arc::new(PinStore::default());
        let first = store.pin(AgentName::new("child"), assembled("a"), source());
        let _second = store.pin(AgentName::new("child"), assembled("b"), source());
        first.remove();
        assert_eq!(
            store
                .get(&AgentName::new("child"))
                .expect("still pinned")
                .agent
                .as_str(),
            "b"
        );
    }
}
