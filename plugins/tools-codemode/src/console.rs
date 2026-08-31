//! Invariant: console output is the ONLY thing a program returns to the model, and it is itself
//! ledgered. The ordered concatenation of a program's `program/console` chunks equals the `run`
//! call's `tool/result` content, modulo the truncation notice.
//!
//! How both halves of that can be true while output STREAMS: the tee emits chunks while the head
//! budget lasts, and once it is spent it keeps only a tail ring. What it never does is retract a
//! chunk it already emitted — so the head the model reads is the head the ledger holds, and the
//! notice plus the tail are emitted once, at the end.

use std::collections::VecDeque;

use bough_plugin_js::ConsoleSink;
use bough_plugin_tools::ToolCallId;

use crate::vocabulary::ProgramConsoleBody;

/// How a dropped middle is named. The count is BYTES, because that is what the cap is in.
pub fn notice(dropped: usize) -> String {
    format!("\n… {dropped} bytes elided …\n")
}

/// The sink `tools-codemode` hands every program: it buffers lines and flushes them as
/// `program/console` steps AS PRODUCED, so the TUI streams a long program rather than waiting.
pub struct ConsoleTee {
    program: ToolCallId,
    max_bytes: usize,
    tx: tokio::sync::mpsc::UnboundedSender<ProgramConsoleBody>,
    state: parking_lot::Mutex<TeeState>,
}

#[derive(Default)]
struct TeeState {
    /// Every chunk emitted so far, in order — the `tool/result` content.
    emitted: Vec<String>,
    chunk: u32,
    head_bytes: usize,
    tail: VecDeque<String>,
    tail_bytes: usize,
    dropped: usize,
    finished: bool,
}

impl ConsoleTee {
    /// The tee, and the receiver a drain task appends `program/console` steps from.
    pub fn new(
        program: ToolCallId,
        max_bytes: usize,
    ) -> (
        ConsoleTee,
        tokio::sync::mpsc::UnboundedReceiver<ProgramConsoleBody>,
    ) {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        (
            ConsoleTee {
                program,
                max_bytes: max_bytes.max(1),
                tx,
                state: parking_lot::Mutex::new(TeeState::default()),
            },
            rx,
        )
    }

    fn head_budget(&self) -> usize {
        // Half the cap streams; the other half is held for the tail, which is where a program's
        // answer usually is.
        self.max_bytes / 2
    }

    fn tail_budget(&self) -> usize {
        self.max_bytes - self.head_budget()
    }

    /// Everything written so far, in order — the `tool/result` content.
    pub fn text(&self) -> String {
        self.state.lock().emitted.concat()
    }

    /// How many bytes the cap dropped.
    pub fn dropped(&self) -> usize {
        self.state.lock().dropped
    }

    /// Emit the notice and the tail. Called ONCE, when the program has ended; after it, `text()`
    /// is final and equals the concatenation of every emitted chunk.
    pub fn finish(&self) {
        let mut state = self.state.lock();
        if state.finished {
            return;
        }
        state.finished = true;
        if state.dropped == 0 && state.tail.is_empty() {
            return;
        }
        let dropped = state.dropped;
        let tail: String = state.tail.iter().cloned().collect::<Vec<_>>().concat();
        state.tail.clear();
        state.tail_bytes = 0;
        if dropped > 0 {
            let text = notice(dropped);
            emit(&mut state, &self.program, &self.tx, text, dropped);
        }
        if !tail.is_empty() {
            emit(&mut state, &self.program, &self.tx, tail, 0);
        }
    }
}

fn emit(
    state: &mut TeeState,
    program: &ToolCallId,
    tx: &tokio::sync::mpsc::UnboundedSender<ProgramConsoleBody>,
    text: String,
    dropped_bytes: usize,
) {
    let chunk = state.chunk;
    state.chunk += 1;
    state.emitted.push(text.clone());
    // The send is unbounded and therefore never blocks the engine's thread; a closed receiver
    // means the drain task is gone, and the chunk is still in `emitted`.
    let _ = tx.send(ProgramConsoleBody {
        program: program.clone(),
        chunk,
        text,
        dropped_bytes,
    });
}

impl ConsoleSink for ConsoleTee {
    fn write(&self, line: &str) {
        let line = if line.ends_with('\n') {
            line.to_string()
        } else {
            format!("{line}\n")
        };
        let mut state = self.state.lock();
        if state.finished {
            return;
        }
        if state.head_bytes + line.len() <= self.head_budget() {
            state.head_bytes += line.len();
            emit(&mut state, &self.program, &self.tx, line, 0);
            return;
        }
        // Past the head budget: keep a tail ring and count what it evicts.
        state.tail_bytes += line.len();
        state.tail.push_back(line);
        let budget = self.tail_budget();
        while state.tail_bytes > budget {
            match state.tail.pop_front() {
                Some(old) => {
                    state.tail_bytes -= old.len();
                    state.dropped += old.len();
                }
                None => break,
            }
        }
    }
}

/// Render `text` under `max_bytes` keeping the HEAD and the TAIL and naming the dropped count —
/// a pure function so the shape is testable without a runtime, and the one the TUI reuses when
/// it re-renders a recorded program.
pub fn truncate(text: &str, max_bytes: usize) -> (String, usize) {
    if text.len() <= max_bytes {
        return (text.to_string(), 0);
    }
    let head_budget = max_bytes / 2;
    let tail_budget = max_bytes - head_budget;
    let head_end = floor_boundary(text, head_budget);
    let tail_start = ceil_boundary(text, text.len().saturating_sub(tail_budget));
    let dropped = tail_start - head_end;
    (
        format!(
            "{}{}{}",
            &text[..head_end],
            notice(dropped),
            &text[tail_start..]
        ),
        dropped,
    )
}

/// The largest char boundary at or below `i`.
fn floor_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// The smallest char boundary at or above `i`.
fn ceil_boundary(s: &str, i: usize) -> usize {
    let mut i = i.min(s.len());
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tee(max: usize) -> ConsoleTee {
        ConsoleTee::new(ToolCallId::new("call_1"), max).0
    }

    #[test]
    fn text_under_the_cap_is_returned_whole() {
        assert_eq!(truncate("hello\n", 64), ("hello\n".to_string(), 0));
    }

    #[test]
    fn the_renderer_keeps_head_and_tail_and_names_the_dropped_bytes() {
        let text = "A".repeat(50) + &"Z".repeat(50);
        let (out, dropped) = truncate(&text, 20);
        assert_eq!(dropped, 80, "100 bytes under a 20-byte cap drops 80");
        assert!(out.starts_with("AAAAAAAAAA"), "{out}");
        assert!(out.ends_with("ZZZZZZZZZZ"), "{out}");
        assert!(out.contains("80 bytes elided"), "{out}");
    }

    #[test]
    fn truncation_never_splits_a_character() {
        let text = "é".repeat(40);
        let (out, dropped) = truncate(&text, 21);
        assert!(dropped > 0);
        assert!(out.contains('é'), "{out}");
    }

    #[test]
    fn the_tee_streams_while_the_head_budget_lasts() {
        let tee = tee(1024);
        tee.write("one");
        tee.write("two");
        tee.finish();
        assert_eq!(tee.text(), "one\ntwo\n");
        assert_eq!(tee.dropped(), 0);
    }

    #[test]
    fn an_overflowing_program_keeps_the_head_the_notice_and_the_tail() {
        // Budget 20 ⇒ 10 bytes of head, 10 of tail. Each line is 6 bytes.
        let tee = tee(20);
        for line in ["aaaaa", "bbbbb", "ccccc", "ddddd", "eeeee"] {
            tee.write(line);
        }
        tee.finish();
        let text = tee.text();
        assert!(text.starts_with("aaaaa\n"), "{text}");
        assert!(text.ends_with("eeeee\n"), "{text}");
        assert!(text.contains("bytes elided"), "{text}");
        assert!(tee.dropped() > 0);
    }

    #[test]
    fn finish_is_idempotent_so_the_chunks_stay_the_concatenation() {
        let tee = tee(20);
        for _ in 0..10 {
            tee.write("xxxxx");
        }
        tee.finish();
        let once = tee.text();
        tee.finish();
        assert_eq!(tee.text(), once, "a second finish must add nothing");
    }
}
