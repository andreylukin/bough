//! SpawnCaps (port of `src/agents/caps.ts`): per-turn 8 and concurrent
//! tree-wide 4 delegation caps; refusal charges nothing; reserve is
//! Mutex-atomic under real concurrency; bus backstop + per-turn GC.
//! STUB (wave 2, row 2.1) — the type is real and lives in `types::HostState`.

use std::collections::HashMap;
use std::sync::Mutex;

/// The delegation cap ledger. Memory-only.
pub struct SpawnCaps {
    #[allow(dead_code)]
    inner: Mutex<HashMap<String, u32>>,
}

impl SpawnCaps {
    pub fn new() -> Self {
        SpawnCaps { inner: Mutex::new(HashMap::new()) }
    }
}

impl Default for SpawnCaps {
    fn default() -> Self {
        Self::new()
    }
}
