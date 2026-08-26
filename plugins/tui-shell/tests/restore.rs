//! V8: the terminal is restored on EVERY exit path. These two are the ones no other test can
//! reach — calling restore twice, and panicking with the hook installed.
//!
//! Both touch PROCESS-WIDE state (the entered-steps flag and the panic hook), so they take one
//! lock: cargo runs a test binary's tests on several threads, and two of these interleaving would
//! make each one assert about the other's flag.

use bough_plugin_tui_shell::term;

static TERMINAL: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

#[test]
fn restore_now_is_idempotent() {
    let _serial = TERMINAL.lock();
    let before = term::restores();
    term::arm_for_test();
    assert!(term::is_entered());

    term::restore_now();
    assert!(!term::is_entered(), "the flag is cleared by the first call");
    assert_eq!(term::restores(), before + 1);

    term::restore_now();
    term::restore_now();
    assert_eq!(
        term::restores(),
        before + 1,
        "a second call restores nothing: it is safe from a panic hook and from Drop at once"
    );
}

#[test]
fn the_panic_hook_restores_before_delegating() {
    let _serial = TERMINAL.lock();
    let observed: std::sync::Arc<parking_lot::Mutex<Option<bool>>> = Default::default();
    let recorder = observed.clone();
    // The "previous" hook records whether the terminal had already been restored when it ran.
    std::panic::set_hook(Box::new(move |_| {
        *recorder.lock() = Some(!term::is_entered());
    }));

    let uninstall = term::install_panic_hook();
    term::arm_for_test();
    let before = term::restores();

    let caught = std::panic::catch_unwind(|| panic!("a pane's render blew up"));
    assert!(caught.is_err());

    assert_eq!(
        term::restores(),
        before + 1,
        "the hook restored the terminal"
    );
    assert_eq!(
        *observed.lock(),
        Some(true),
        "and it had already restored by the time the previous hook ran"
    );

    uninstall();
    let _ = std::panic::take_hook();
}
