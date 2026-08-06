//! LLM request tracing (port of `src/llm/trace.ts`). v1 STUB (plan row 1.11):
//! `with_trace(inner, None)` is the identity — pinned by test — and a label
//! is only ever produced when `BOUGH_TRACE_DIR` is set, which normal use
//! never does. Wave 3.16 un-stubs the writer (`n` counts failed attempts;
//! prefix sha emitted once per tier; all fs errors swallowed).
//!
//! Composition order (load-bearing, `client_for`): trace sits INSIDE the
//! retries so a recorded trace shows each attempt, and outside pricing so a
//! recorded round already carries its cost.

use std::sync::Arc;

use crate::llm::routing::Env;
use crate::types::LlmClient;

/// Where one turn's raw provider I/O goes.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceLabel {
    pub dir: String,
    pub session_id: String,
    pub turn_id: String,
}

/// `BOUGH_TRACE_DIR` (trimmed) set → a label; unset or blank → `None` — off
/// unless asked, no sink, no cost.
pub fn trace_label(session_id: &str, turn_id: &str, env: &Env) -> Option<TraceLabel> {
    let dir = env("BOUGH_TRACE_DIR")?;
    let dir = dir.trim();
    if dir.is_empty() {
        return None;
    }
    Some(TraceLabel {
        dir: dir.to_string(),
        session_id: session_id.to_string(),
        turn_id: turn_id.to_string(),
    })
}

/// `label == None` → the client comes back identity-untouched (test-pinned).
/// v1 also passes a labelled client through unchanged: tracing is diagnostic
/// only and graceful absence is the documented contract until wave 3 ports
/// the JSONL writer.
pub fn with_trace(inner: Arc<dyn LlmClient>, label: Option<TraceLabel>) -> Arc<dyn LlmClient> {
    let _ = label;
    inner
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::test_support::fake_client;

    #[test]
    fn tracing_off_returns_the_client_untouched() {
        let (inner, _calls) = fake_client(vec![]);
        let wrapped = with_trace(inner.clone(), None);
        assert!(Arc::ptr_eq(&inner, &wrapped), "with_trace(inner, None) must be the identity");
    }

    #[test]
    fn trace_label_reads_bough_trace_dir_trimmed() {
        let none: Env = Arc::new(|_| None);
        assert_eq!(trace_label("s", "t", &none), None);
        let blank: Env = Arc::new(|_| Some("  ".into()));
        assert_eq!(trace_label("s", "t", &blank), None, "a blank dir is not a directory");
        let set: Env = Arc::new(|k| (k == "BOUGH_TRACE_DIR").then(|| "/tmp/x".to_string()));
        assert_eq!(
            trace_label("s", "t", &set),
            Some(TraceLabel { dir: "/tmp/x".into(), session_id: "s".into(), turn_id: "t".into() })
        );
    }
}
