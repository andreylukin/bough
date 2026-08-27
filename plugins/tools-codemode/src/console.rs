//! Invariant: console output is the ONLY thing a program returns to the model, and it is itself
//! ledgered. The ordered concatenation of a program's `program/console` chunks equals the `run`
//! call's `tool/result` content, modulo the truncation notice.

use bough_plugin_js::ConsoleSink;
use bough_plugin_tools::ToolCallId;

/// The sink `tools-codemode` hands every program: it buffers lines and flushes them as
/// `program/console` steps AS PRODUCED, so the TUI streams a long program rather than waiting.
pub struct ConsoleTee {
    #[allow(dead_code)]
    program: ToolCallId,
    #[allow(dead_code)]
    max_bytes: usize,
}

impl ConsoleTee {
    pub fn new(program: ToolCallId, max_bytes: usize) -> ConsoleTee {
        ConsoleTee {
            program,
            max_bytes,
        }
    }

    /// Everything written so far, in order — the `tool/result` content.
    ///
    /// WP-2 owns the body.
    pub fn text(&self) -> String {
        todo!("WP-2: the ordered concatenation of the flushed chunks")
    }

    /// How many bytes the cap dropped.
    ///
    /// WP-2 owns the body.
    pub fn dropped(&self) -> usize {
        todo!("WP-2: bytes elided by the head/tail truncation")
    }
}

impl ConsoleSink for ConsoleTee {
    fn write(&self, _line: &str) {
        todo!("WP-2: buffer, flush a program/console step, never block the engine's thread")
    }
}

/// Render `text` under `max_bytes` keeping the HEAD and the TAIL and naming the dropped count —
/// a pure function so the shape is testable without a runtime.
///
/// WP-2 owns the body.
pub fn truncate(_text: &str, _max_bytes: usize) -> (String, usize) {
    todo!("WP-2: head+tail truncation with an explicit `… N bytes elided …` notice")
}
