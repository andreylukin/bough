//! `ask()` holds (port of `src/hostfn/ask.ts`). Memory-only — "the hold dies
//! with the turn"; the durable record is the settled AskPart. Decline rejects
//! with the catchable `user declined to answer:` prefix; a settled-race is a
//! 409. STUB (wave 2, row 2.5) — the registry type is real and lives in
//! `types::HostState`.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::schema::parts::AskQuestion;

/// Live `ask()` holds, keyed by question id. Memory-only.
pub struct AskRegistry {
    #[allow(dead_code)]
    inner: Mutex<HashMap<String, AskQuestion>>,
}

impl AskRegistry {
    pub fn new() -> Self {
        AskRegistry { inner: Mutex::new(HashMap::new()) }
    }

    /// Pending questions — `GET /questions` reconnect path. Stub: empty.
    pub fn pending(&self) -> Vec<AskQuestion> {
        Vec::new()
    }
}

impl Default for AskRegistry {
    fn default() -> Self {
        Self::new()
    }
}
