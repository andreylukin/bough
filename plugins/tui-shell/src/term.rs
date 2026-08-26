//! Invariant: `enter` REMEMBERS EVERY STEP IT COMPLETED, so `leave` undoes exactly what `enter`
//! did and no more; and `restore_now` is idempotent, synchronous, allocation-free and safe from a
//! panic hook. This is the one module that touches the real terminal, and it is the reason V8's
//! three assertions (normal screen back, cursor visible, raw mode off) can hold on every exit
//! path — clean quit, boot failure, panic, SIGINT.

use crate::{TuiConfig, TuiError};

/// The entered terminal. Dropping it leaves exactly the states `enter` set.
pub struct TerminalGuard {
    _private: (),
}

impl TerminalGuard {
    /// raw mode → alt screen → mouse capture → bracketed paste → hide cursor, in that order.
    /// Every step it completes is remembered, so `leave` undoes exactly what `enter` did.
    pub fn enter(_cfg: &TuiConfig) -> Result<TerminalGuard, TuiError> {
        todo!("WP-2")
    }

    /// Undo, in reverse order, exactly the steps `enter` completed.
    pub fn leave(&self) {
        todo!("WP-2")
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        self.leave();
    }
}

/// Idempotent, synchronous, allocation-free, safe from a panic hook and safe to call twice:
/// show cursor → disable bracketed paste → disable mouse capture → leave alt screen →
/// disable raw mode. Guarded by a process-wide `AtomicBool`.
pub fn restore_now() {
    todo!("WP-2")
}

/// Chains to the previous hook AFTER `restore_now()`, so a panic message lands on the normal
/// screen. Returns an inverse that reinstalls the previous hook.
pub fn install_panic_hook() -> impl FnOnce() {
    || todo!("WP-2")
}

/// Whether stdout is a real terminal. The input to `Backend::Auto` (P3-D2).
pub fn stdout_is_tty() -> bool {
    todo!("WP-2")
}
