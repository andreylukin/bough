//! Invariant: `enter` REMEMBERS EVERY STEP IT COMPLETED, so `leave` undoes exactly what `enter`
//! did and no more; and `restore_now` is idempotent, synchronous, allocation-free and safe from a
//! panic hook. This is the one module that touches the real terminal, and it is the reason V8's
//! three assertions (normal screen back, cursor visible, raw mode off) can hold on every exit
//! path — clean quit, boot failure, panic, SIGINT.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use crate::{TuiConfig, TuiError};

/// Which steps of `enter` completed, as a bitmask. A step that failed is not in it, so its inverse
/// never runs: undoing something that was never done is how a terminal ends up in a state neither
/// side asked for.
const RAW: u8 = 1 << 0;
const ALT: u8 = 1 << 1;
const MOUSE: u8 = 1 << 2;
const PASTE: u8 = 1 << 3;
const HIDDEN: u8 = 1 << 4;

/// The steps currently in effect, process-wide. `restore_now` reads and clears it, which is what
/// makes it idempotent from a panic hook, from `Drop` and from the launcher's teardown at once.
static ENTERED: AtomicU8 = AtomicU8::new(0);
/// Set by [`arm_for_test`]: the bookkeeping runs, the escape sequences do not.
static DRY: AtomicBool = AtomicBool::new(false);
/// How many times the restore body actually ran. The idempotence test's observer.
static RESTORES: AtomicUsize = AtomicUsize::new(0);

/// The entered terminal. Dropping it leaves exactly the states `enter` set.
pub struct TerminalGuard {
    /// Purely a marker: the real bookkeeping is [`ENTERED`], because a panic hook has no guard.
    _private: (),
}

impl TerminalGuard {
    /// raw mode → alt screen → mouse capture → bracketed paste → hide cursor, in that order.
    /// Every step it completes is remembered, so `leave` undoes exactly what `enter` did.
    pub fn enter(cfg: &TuiConfig) -> Result<TerminalGuard, TuiError> {
        use crossterm::{cursor, event, execute, terminal};
        let mut out = std::io::stdout();
        let fail = |step: &'static str, source: std::io::Error| {
            // Whatever HAD been done is undone before the error leaves: a half-entered terminal is
            // the state V8 exists to make impossible.
            restore_now();
            TuiError::Terminal { step, source }
        };

        terminal::enable_raw_mode().map_err(|e| fail("raw mode", e))?;
        ENTERED.fetch_or(RAW, Ordering::SeqCst);

        execute!(out, terminal::EnterAlternateScreen).map_err(|e| fail("alt screen", e))?;
        ENTERED.fetch_or(ALT, Ordering::SeqCst);

        if cfg.mouse {
            execute!(out, event::EnableMouseCapture).map_err(|e| fail("mouse capture", e))?;
            ENTERED.fetch_or(MOUSE, Ordering::SeqCst);
        }

        execute!(out, event::EnableBracketedPaste).map_err(|e| fail("bracketed paste", e))?;
        ENTERED.fetch_or(PASTE, Ordering::SeqCst);

        execute!(out, cursor::Hide).map_err(|e| fail("hide cursor", e))?;
        ENTERED.fetch_or(HIDDEN, Ordering::SeqCst);

        let _ = out.flush();
        Ok(TerminalGuard { _private: () })
    }

    /// Undo, in reverse order, exactly the steps `enter` completed.
    pub fn leave(&self) {
        restore_now();
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Idempotent, synchronous, allocation-free, safe from a panic hook and safe to call twice:
/// show cursor → disable bracketed paste → disable mouse capture → leave alt screen →
/// disable raw mode. Guarded by a process-wide flag.
pub fn restore_now() {
    let done = ENTERED.swap(0, Ordering::SeqCst);
    if done == 0 {
        return;
    }
    RESTORES.fetch_add(1, Ordering::SeqCst);
    if DRY.load(Ordering::SeqCst) {
        return;
    }
    use crossterm::{cursor, event, execute, terminal};
    let mut out = std::io::stdout();
    if done & HIDDEN != 0 {
        let _ = execute!(out, cursor::Show);
    }
    if done & PASTE != 0 {
        let _ = execute!(out, event::DisableBracketedPaste);
    }
    if done & MOUSE != 0 {
        let _ = execute!(out, event::DisableMouseCapture);
    }
    if done & ALT != 0 {
        let _ = execute!(out, terminal::LeaveAlternateScreen);
    }
    if done & RAW != 0 {
        let _ = terminal::disable_raw_mode();
    }
    let _ = out.flush();
}

/// Whether the terminal is currently entered.
pub fn is_entered() -> bool {
    ENTERED.load(Ordering::SeqCst) != 0
}

/// How many times [`restore_now`] has actually restored something. Observability for the
/// idempotence test and for the launcher's teardown log.
pub fn restores() -> usize {
    RESTORES.load(Ordering::SeqCst)
}

/// Pretend the terminal was entered, without touching one. Only a test calls this; a real `enter`
/// is what arms the flag in a running process.
#[doc(hidden)]
pub fn arm_for_test() {
    DRY.store(true, Ordering::SeqCst);
    ENTERED.store(RAW | ALT | MOUSE | PASTE | HIDDEN, Ordering::SeqCst);
}

/// Chains to the previous hook AFTER `restore_now()`, so a panic message lands on the normal
/// screen. Returns an inverse that reinstalls the previous hook.
pub fn install_panic_hook() -> impl FnOnce() {
    let previous = std::panic::take_hook();
    // The previous hook is shared with the inverse, so reinstalling it is a move out of an
    // `Option` rather than a second `take_hook` (which would take OUR hook back).
    let shared = std::sync::Arc::new(previous);
    let for_hook = shared.clone();
    std::panic::set_hook(Box::new(move |info| {
        restore_now();
        for_hook(info);
    }));
    move || {
        // Drop our hook and put the previous one back, whatever it was.
        let _ = std::panic::take_hook();
        // `Err` means a panic is in flight and still holds the hook; leaving the default is then
        // the only safe answer, and `take_hook` above already installed it.
        if let Ok(previous) = std::sync::Arc::try_unwrap(shared) {
            std::panic::set_hook(previous);
        }
    }
}

/// Whether stdout is a real terminal. The input to `Backend::Auto` (P3-D2).
pub fn stdout_is_tty() -> bool {
    crossterm::tty::IsTty::is_tty(&std::io::stdout())
}
