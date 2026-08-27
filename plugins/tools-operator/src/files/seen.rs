//! Invariant: `[path#]` means "the version you just viewed or wrote". The map holds the tag AND
//! the text, because a rebase re-checks the actual lines rather than trusting the tag.

use std::collections::BTreeMap;
use std::path::PathBuf;

use bough_plugin_ledger::AgentName;

/// What `view`/`write` remember, per `(agent, path)`.
pub struct SeenFiles(pub parking_lot::Mutex<BTreeMap<(AgentName, PathBuf), (String, String)>>);

impl Default for SeenFiles {
    fn default() -> SeenFiles {
        SeenFiles(parking_lot::Mutex::new(BTreeMap::new()))
    }
}

impl SeenFiles {
    /// Remember the text an agent just saw or wrote, and its tag.
    pub fn remember(&self, agent: AgentName, path: PathBuf, tag: String, text: String) {
        self.0.lock().insert((agent, path), (tag, text));
    }

    /// The `(tag, text)` this agent last saw for `path`.
    pub fn recall(&self, agent: &AgentName, path: &PathBuf) -> Option<(String, String)> {
        self.0.lock().get(&(agent.clone(), path.clone())).cloned()
    }
}
