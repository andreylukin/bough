//! The shape of the prompt a session last actually sent.
//!
//! THE QUESTION THIS ANSWERS: "what is in my context window, and what is it
//! costing me?" Every turn already computes the answer —
//! [`crate::prompt::assemble::assemble_prompt`] returns the included sections
//! with a sha and a length for each — and until this module existed the runner
//! dropped it on the floor unless `BOUGH_TRACE_DIR` was set. So the harness
//! knew precisely what it had injected, wrote it to a file nobody had turned
//! on, and showed the user a single percentage.
//!
//! WHY A MEMO AND NOT A COLUMN. This describes the LAST turn, not the session:
//! it is re-derived from disk every turn (rules are re-read, the skill catalog
//! is re-listed), so persisting it would mean storing a value that is stale the
//! moment the next turn starts. A process-local memo is exactly as durable as
//! the fact it holds. A session the server has not run a turn for since boot
//! reports nothing, which is the truth — the alternative is showing a shape
//! from a previous process as if it were current.
//!
//! Capped like the other per-session memos in this module tree, and cleared by
//! the same test seam.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use serde::{Deserialize, Serialize};

use crate::prompt::assemble::{AssembledPrompt, SectionSha};

/// One session's last prompt, as the UI reads it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptShape {
    /// Every included section, in prompt order — stable tier then volatile.
    pub sections: Vec<SectionSha>,
    /// The cacheable prefix's length, shared across sessions of this shape.
    pub stable_bytes: usize,
    /// The per-session suffix's length, paid by this session alone.
    pub volatile_bytes: usize,
}

static LAST: LazyLock<Mutex<HashMap<String, PromptShape>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// How many sessions' shapes are kept. Same bound and same argument as
/// `project.rs`'s memos: a long-lived server must not grow a row per session
/// it has ever run, and an occasionally-recomputed shape costs one assembly.
const MEMO_CAP: usize = 512;

/// Record what this turn sent. Called from the runner, right where the prompt
/// is assembled — the only place both halves are in hand.
pub fn remember(session_id: &str, prompt: &AssembledPrompt) {
    let shape = PromptShape {
        sections: prompt.shas.clone(),
        stable_bytes: prompt.system.len(),
        volatile_bytes: prompt.system_volatile.len(),
    };
    let mut map = LAST.lock().unwrap();
    if map.len() >= MEMO_CAP {
        map.clear();
    }
    map.insert(session_id.to_string(), shape);
}

/// The last shape for a session, or `None` when it has not run a turn in this
/// process. `None` is a real answer and must be rendered as one.
pub fn last(session_id: &str) -> Option<PromptShape> {
    LAST.lock().unwrap().get(session_id).cloned()
}

/// Test seam: forget every session's shape.
pub fn reset() {
    LAST.lock().unwrap().clear();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::protocol::HostFnName;
    use crate::prompt::assemble::{assemble_prompt, PromptInput, SectionId};
    use crate::schema::parts::SessionKind;

    #[test]
    fn a_sessions_last_prompt_is_readable_with_a_size_per_section() {
        reset();
        assert_eq!(last("s1"), None, "nothing ran yet is not an empty prompt");

        let mut input = PromptInput::new(SessionKind::Root, [HostFnName::Bash]);
        input.notes = vec!["## Workspace\n\n/checkouts/acme".to_string()];
        let prompt = assemble_prompt(&input);
        remember("s1", &prompt);

        let shape = last("s1").unwrap();
        assert_eq!(shape.stable_bytes, prompt.system.len());
        assert_eq!(shape.volatile_bytes, prompt.system_volatile.len());
        // Every section carries its own length, and they are not all zero —
        // the sha alone could never say what a section costs.
        assert!(shape.sections.iter().all(|s| s.bytes > 0), "{shape:?}");
        // The volatile tier's sections are the ones marked volatile.
        let notes = shape
            .sections
            .iter()
            .find(|s| s.id == SectionId::Notes)
            .unwrap();
        assert!(notes.id.is_volatile());
        assert!(shape.sections.iter().any(|s| !s.id.is_volatile()));
        reset();
    }

    /// The memo describes the LAST turn, so a second turn replaces the first
    /// rather than accumulating beside it.
    #[test]
    fn a_later_turn_replaces_the_shape_it_supersedes() {
        reset();
        let bare = assemble_prompt(&PromptInput::new(SessionKind::Root, [HostFnName::Bash]));
        remember("s2", &bare);
        let before = last("s2").unwrap().volatile_bytes;

        let mut input = PromptInput::new(SessionKind::Root, [HostFnName::Bash]);
        input.notes = vec!["## Workspace\n\n/checkouts/acme".to_string()];
        remember("s2", &assemble_prompt(&input));

        let after = last("s2").unwrap();
        assert!(after.volatile_bytes > before);
        assert_eq!(LAST.lock().unwrap().len(), 1);
        reset();
    }
}
