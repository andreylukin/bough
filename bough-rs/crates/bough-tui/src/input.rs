//! What remains of `src/tui/mouse.ts` under crossterm: the enter/leave
//! sequences crossterm does not own, the idempotent terminal-restore guard
//! (RAII + panic hook — the port of "the terminal is restored on every exit
//! path"), and the `CSI 27;m;k~` modifyOtherKeys decode crossterm misses.
//!
//! NOT the alternate screen: `?1049h` is the one sequence that is not
//! idempotent, and the renderer (ratatui/crossterm) owns it — the split is the
//! same one the TS tree wrote down. `enter_tui` pushes the title and switches
//! on SGR mouse tracking, bracketed paste and focus reporting; `leave_tui`
//! undoes them, restores the cursor, and pops the title, and it is safe to run
//! any number of times on any path including a panic.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// SGR mouse tracking + bracketed paste + focus reporting + the title push.
/// NOT `?1049h` — the renderer owns the alternate screen.
pub const ENTER_SEQ: &str =
    "\u{1b}[22;0t\u{1b}[?1000h\u{1b}[?1002h\u{1b}[?1006h\u{1b}[?2004h\u{1b}[?1004h";

/// Mouse, paste and focus modes off, cursor visible, the pushed title popped
/// back. `?25h` stays even though the renderer shows the cursor too: it is
/// idempotent, and it is the only restore on a path where the renderer's
/// teardown never ran. Must run AFTER renderer teardown (the renderer blanks
/// the title on exit).
pub const LEAVE_SEQ: &str =
    "\u{1b}[?1004l\u{1b}[?2004l\u{1b}[?1006l\u{1b}[?1002l\u{1b}[?1000l\u{1b}[?25h\u{1b}[23;0t";

fn write_stdout(seq: &str) {
    // stdout already gone (exiting) — there is nothing left to signal to.
    let _ = std::io::stdout().write_all(seq.as_bytes());
    let _ = std::io::stdout().flush();
}

pub fn enter_tui() {
    write_stdout(ENTER_SEQ);
}

/// `cleanup` clears whatever sticky state `term.rs` set (progress, tab tint).
/// A parameter rather than an import, so a caller that never built a Term can
/// still leave. Leaving must not panic: this runs on the exit path too.
pub fn leave_tui(cleanup: Option<&dyn Fn()>) {
    if let Some(cleanup) = cleanup {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup));
    }
    write_stdout(LEAVE_SEQ);
}

type Cleanup = Box<dyn Fn() + Send + Sync>;

struct GuardInner {
    left: AtomicBool,
    cleanup: Mutex<Option<Cleanup>>,
    write: Box<dyn Fn(&str) + Send + Sync>,
}

impl GuardInner {
    fn leave(&self) {
        // Idempotent: only the FIRST leave writes. Every exit path — normal
        // return, early error, panic hook, Drop — calls this; exactly one wins.
        if self.left.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(cleanup) = self.cleanup.lock().ok().and_then(|mut c| c.take()) {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(cleanup));
        }
        (self.write)(LEAVE_SEQ);
    }
}

/// The terminal-restore guard. Construct it right after `enter_tui`; the
/// terminal is then restored on EVERY exit path:
/// - explicit `guard.leave()` on the normal path,
/// - `Drop` when the scope unwinds,
/// - the installed panic hook, which fires BEFORE unwinding reaches Drop —
///   and the idempotence flag keeps the later Drop from writing twice.
pub struct TuiGuard {
    inner: Arc<GuardInner>,
}

static PANIC_GUARDS: OnceLock<Mutex<Vec<Arc<GuardInner>>>> = OnceLock::new();
static HOOK_INSTALLED: AtomicBool = AtomicBool::new(false);

impl TuiGuard {
    /// Production guard: writes to stdout, no term cleanup.
    pub fn new() -> TuiGuard {
        TuiGuard::with_writer(Box::new(write_stdout), None)
    }

    /// Injected writer (tests) and optional sticky-state cleanup.
    pub fn with_writer(
        write: Box<dyn Fn(&str) + Send + Sync>,
        cleanup: Option<Cleanup>,
    ) -> TuiGuard {
        let inner = Arc::new(GuardInner {
            left: AtomicBool::new(false),
            cleanup: Mutex::new(cleanup),
            write,
        });
        TuiGuard { inner }
    }

    /// Register this guard with the process panic hook, so a panic anywhere
    /// restores the terminal BEFORE the default hook prints the message onto
    /// a readable screen. Installing twice is safe; the hook chains.
    pub fn install_panic_hook(&self) {
        let guards = PANIC_GUARDS.get_or_init(|| Mutex::new(Vec::new()));
        if let Ok(mut g) = guards.lock() {
            g.push(Arc::clone(&self.inner));
        }
        if !HOOK_INSTALLED.swap(true, Ordering::SeqCst) {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                if let Some(guards) = PANIC_GUARDS.get() {
                    if let Ok(g) = guards.lock() {
                        for guard in g.iter() {
                            guard.leave();
                        }
                    }
                }
                previous(info);
            }));
        }
    }

    /// Leave now, on the normal path. Safe to call any number of times.
    pub fn leave(&self) {
        self.inner.leave();
    }

    pub fn has_left(&self) -> bool {
        self.inner.left.load(Ordering::SeqCst)
    }
}

impl Default for TuiGuard {
    fn default() -> Self {
        TuiGuard::new()
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        self.inner.leave();
    }
}

// ---------------------------------------------------------------------------
// modifyOtherKeys decode (`CSI 27;mods;code~`) — crossterm gap
// ---------------------------------------------------------------------------

/// Decode one `CSI 27;mods;code~` report into the bytes the key layer expects.
/// `mods` is a 1-based bitfield (bit0 shift, bit1 alt, bit2 ctrl, bit3 super);
/// ctrl folds letters to C0; alt prefixes ESC; undecodable ⇒ `""` (swallowed,
/// never typed).
pub fn decode_modify_other(mods: u32, code: u32) -> String {
    let bits = mods.saturating_sub(1);
    let alt = bits & 2 != 0;
    let ctrl = bits & 4 != 0;
    if !(1..=0x10ffff).contains(&code) {
        return String::new();
    }
    let mut base: char = match code {
        13 => '\r',
        9 => '\t',
        27 => '\u{1b}',
        127 | 8 => '\u{7f}',
        32..=126 => char::from_u32(code).unwrap(),
        _ => return String::new(),
    };
    // Ctrl folds a letter down to its C0 byte.
    if ctrl && base.is_ascii_lowercase() {
        base = char::from_u32(base as u32 - 96).unwrap();
    } else if ctrl && base.is_ascii_uppercase() {
        base = char::from_u32(base as u32 - 64).unwrap();
    }
    if alt {
        format!("\u{1b}{base}")
    } else {
        base.to_string()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    #[test]
    fn enter_pushes_the_title_and_mouse_modes_but_never_the_alternate_screen() {
        assert!(ENTER_SEQ.starts_with("\u{1b}[22;0t"), "title push first");
        for mode in ["?1000h", "?1002h", "?1006h", "?2004h", "?1004h"] {
            assert!(ENTER_SEQ.contains(mode), "{mode}");
        }
        // NOT ?1049h — the renderer owns the alternate screen.
        assert!(!ENTER_SEQ.contains("1049"));
    }

    #[test]
    fn leave_undoes_every_mode_shows_the_cursor_and_pops_the_title() {
        for mode in ["?1004l", "?2004l", "?1006l", "?1002l", "?1000l", "?25h"] {
            assert!(LEAVE_SEQ.contains(mode), "{mode}");
        }
        assert!(LEAVE_SEQ.ends_with("\u{1b}[23;0t"), "title pop last");
        assert!(!LEAVE_SEQ.contains("1049"));
    }

    #[test]
    fn the_guard_is_idempotent_on_every_exit_path() {
        let writes = Arc::new(AtomicUsize::new(0));
        let cleanups = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&writes);
        let c = Arc::clone(&cleanups);
        {
            let guard = TuiGuard::with_writer(
                Box::new(move |seq| {
                    assert_eq!(seq, LEAVE_SEQ);
                    w.fetch_add(1, Ordering::SeqCst);
                }),
                Some(Box::new(move || {
                    c.fetch_add(1, Ordering::SeqCst);
                })),
            );
            // Explicit leave, twice — then Drop fires too.
            guard.leave();
            guard.leave();
            assert!(guard.has_left());
        }
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "exactly one restore, however many paths ran"
        );
        assert_eq!(cleanups.load(Ordering::SeqCst), 1, "cleanup runs once");
    }

    #[test]
    fn a_panicking_cleanup_does_not_stop_the_restore() {
        let writes = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&writes);
        let guard = TuiGuard::with_writer(
            Box::new(move |_| {
                w.fetch_add(1, Ordering::SeqCst);
            }),
            Some(Box::new(|| panic!("sticky-state cleanup exploded"))),
        );
        guard.leave();
        assert_eq!(writes.load(Ordering::SeqCst), 1, "the restore still ran");
    }

    #[test]
    fn the_panic_hook_restores_the_terminal_and_the_later_drop_stays_silent() {
        let writes = Arc::new(AtomicUsize::new(0));
        let w = Arc::clone(&writes);
        let guard = TuiGuard::with_writer(
            Box::new(move |_| {
                w.fetch_add(1, Ordering::SeqCst);
            }),
            None,
        );
        guard.install_panic_hook();
        // Simulate the panic path: the hook body calls leave() on registered
        // guards. (A real panic in-test would spam the harness; the hook and
        // this call run the same code.)
        let result = std::panic::catch_unwind(|| panic!("render exploded"));
        assert!(result.is_err());
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "the hook restored the terminal"
        );
        drop(guard);
        assert_eq!(
            writes.load(Ordering::SeqCst),
            1,
            "Drop after the hook is a no-op"
        );
    }

    #[test]
    fn leave_tui_swallows_a_throwing_cleanup() {
        // Must not panic.
        leave_tui(Some(&|| panic!("cleanup exploded")));
    }

    #[test]
    fn decode_modify_other_folds_ctrl_prefixes_alt_and_swallows_the_undecodable() {
        // Plain enter (mods=1 means no modifiers).
        assert_eq!(decode_modify_other(1, 13), "\r");
        // Alt+Enter — the sequence that once typed itself into the draft.
        assert_eq!(decode_modify_other(3, 13), "\u{1b}\r");
        // Ctrl+a folds to C0.
        assert_eq!(decode_modify_other(5, 97), "\u{1}");
        assert_eq!(decode_modify_other(5, 65), "\u{1}");
        // Alt+x.
        assert_eq!(decode_modify_other(3, 120), "\u{1b}x");
        // Tab / esc / backspace byte forms.
        assert_eq!(decode_modify_other(1, 9), "\t");
        assert_eq!(decode_modify_other(1, 27), "\u{1b}");
        assert_eq!(decode_modify_other(1, 127), "\u{7f}");
        assert_eq!(decode_modify_other(1, 8), "\u{7f}");
        // A function-key code has no byte form and is dropped, not typed.
        assert_eq!(decode_modify_other(1, 0), "");
        assert_eq!(decode_modify_other(1, 200), "");
        assert_eq!(decode_modify_other(1, 0x110000), "");
    }
}
