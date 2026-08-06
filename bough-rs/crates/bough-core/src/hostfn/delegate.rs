//! Delegation host fns (port of `src/hostfn/delegate.ts`): tier-graded grants
//! (`agent`/`spawn`/`join`/`adopt`), interrupt cascade reaches blocking not
//! detached, explicit stop reaches detached, adopt checks lineage. STUB
//! (wave 2, row 2.4) — the registry type is real and lives in
//! `types::HostState`.

use std::collections::HashMap;
use std::sync::Mutex;

/// Detached subagent results awaiting `join()`/timeout. Memory-only.
pub struct DetachedSubagents {
    #[allow(dead_code)]
    inner: Mutex<HashMap<String, String>>,
}

impl DetachedSubagents {
    pub fn new() -> Self {
        DetachedSubagents { inner: Mutex::new(HashMap::new()) }
    }
}

impl Default for DetachedSubagents {
    fn default() -> Self {
        Self::new()
    }
}
